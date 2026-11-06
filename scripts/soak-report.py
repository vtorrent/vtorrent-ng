#!/usr/bin/env python3
"""Turn a soak's raw evidence into a sign-off block.

Reads the sampler's JSONL plus, optionally, the fleet's Prometheus, and prints a
markdown summary with explicit pass/fail lines. Every number is computed from
the recorded data so the assessment cannot drift from the evidence.

Usage:
    scripts/soak-report.py --samples /tmp/opencode/run/samples.jsonl \\
        --prometheus http://127.0.0.1:23961 --label '35ea52e (tuned)' \\
        --hours 168

Only the standard library is used.
"""

import argparse
import datetime
import json
import pathlib
import statistics
import sys
import urllib.parse
import urllib.request

BUDGET_MIB = 256


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--samples', required=True, type=pathlib.Path)
    parser.add_argument('--prometheus', help='base URL, e.g. http://127.0.0.1:23961')
    parser.add_argument('--label', default='soak')
    parser.add_argument('--hours', type=float, default=168.0)
    parser.add_argument('--budget-mib', type=float, default=BUDGET_MIB)
    parser.add_argument('--expected-interval', type=float, default=60.0,
                        help='sampler period in seconds')
    return parser.parse_args()


def load_samples(path):
    rows = []
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        row = json.loads(line)
        stamp = row.get('timestamp') or row.get('time')
        if not stamp:
            raise SystemExit(f'sample without a timestamp in {path}')
        row['_ts'] = datetime.datetime.fromisoformat(stamp.replace('Z', '+00:00'))
        rows.append(row)
    if not rows:
        raise SystemExit(f'no samples in {path}')
    rows.sort(key=lambda r: r['_ts'])
    return rows


def query(base, expr, start, end, step=60):
    params = urllib.parse.urlencode({
        'query': expr,
        'start': start.timestamp(),
        'end': end.timestamp(),
        'step': step,
    })
    with urllib.request.urlopen(f'{base}/api/v1/query_range?{params}', timeout=30) as response:
        payload = json.load(response)
    if payload.get('status') != 'success':
        raise SystemExit(f'prometheus query failed: {payload}')
    return payload['data']['result']


def query_instant(base, expr):
    params = urllib.parse.urlencode({'query': expr})
    with urllib.request.urlopen(f'{base}/api/v1/query?{params}', timeout=30) as response:
        payload = json.load(response)
    if payload.get('status') != 'success':
        raise SystemExit(f'prometheus query failed: {payload}')
    return payload['data']['result']


def series_by_instance(result):
    out = {}
    for series in result:
        instance = series['metric'].get('instance', '?').split(':')[0]
        out[instance] = [(float(t), float(v)) for t, v in series.get('values', [])]
    return out


def single_value(result):
    if not result:
        return None
    return float(result[0]['value'][1])


def compress_intervals(timestamps, tolerance):
    """Collapse timestamps that are close together into (start, end) runs."""
    runs = []
    start = prev = None
    for t in timestamps:
        if start is None:
            start = prev = t
        elif t - prev > tolerance:
            runs.append((start, prev))
            start = prev = t
        else:
            prev = t
    if start is not None:
        runs.append((start, prev))
    return runs


def fmt_time(ts):
    return datetime.datetime.fromtimestamp(ts, datetime.timezone.utc).strftime('%Y-%m-%d %H:%MZ')


def main():
    args = parse_args()
    rows = load_samples(args.samples)
    start, end = rows[0]['_ts'], rows[-1]['_ts']
    span_hours = (end - start).total_seconds() / 3600
    expected = int(span_hours * 3600 / args.expected_interval) + 1

    # A few seconds of jitter is normal; treat 2.5x the cadence as a real gap.
    gap_limit = args.expected_interval * 2.5
    gaps = [
        (prev['_ts'], row['_ts'])
        for prev, row in zip(rows, rows[1:])
        if (row['_ts'] - prev['_ts']).total_seconds() > gap_limit
    ]

    node_count = max((len(r.get('nodes', [])) for r in rows), default=0)
    per_node = []
    for index in range(node_count):
        rss, heights, staked, restarts = [], [], [], 0
        for row in rows:
            node = row['nodes'][index] if index < len(row.get('nodes', [])) else {}
            if node.get('rss_kib'):
                rss.append(node['rss_kib'] / 1024)
            info = node.get('info') or {}
            if isinstance(info.get('block_height'), int):
                heights.append(info['block_height'])
            if isinstance(node.get('restarts'), int):
                restarts = max(restarts, node['restarts'])
            if isinstance(info.get('blocks_staked_total'), int):
                staked.append(info['blocks_staked_total'])
        per_node.append({
            'name': f'node{index + 1}',
            'samples': len(rss),
            'peak': max(rss) if rss else 0.0,
            'median': statistics.median(rss) if rss else 0.0,
            'last': rss[-1] if rss else 0.0,
            'over_budget': sum(1 for v in rss if v > args.budget_mib),
            'height': heights[-1] if heights else None,
            'restarts': restarts,
        })

    report = []
    report.append(f'## Soak report — {args.label}')
    report.append('')
    report.append(f'- Window: **{fmt_time(start.timestamp())} → {fmt_time(end.timestamp())}** '
                  f'({span_hours:.1f} h)')
    report.append(f'- Samples: **{len(rows)}** (expected ~{expected} at '
                  f'{args.expected_interval:.0f}s cadence)')
    if gaps:
        worst = max((b - a).total_seconds() / 60 for a, b in gaps)
        report.append(f'- Sampler gaps over 2.5× cadence: **{len(gaps)}**, longest '
                      f'**{worst:.0f} min**')
    else:
        report.append('- Sampler gaps over 2.5× cadence: **0**')

    report.append('')
    report.append(f'| Node | Height | RSS peak | RSS median | RSS last | Samples over '
                  f'{args.budget_mib:.0f} MiB | Restarts |')
    report.append('|---|---|---|---|---|---|---|')
    for node in per_node:
        report.append(
            f"| {node['name']} | {node['height']} | {node['peak']:.1f} MiB | "
            f"{node['median']:.1f} MiB | {node['last']:.1f} MiB | "
            f"{node['over_budget']} | {node['restarts']} |")

    if args.prometheus:
        base = args.prometheus.rstrip('/')
        budget = args.budget_mib * 1024 * 1024
        report.append('')
        report.append('### From Prometheus')
        report.append('')
        peaks = single_value(query_instant(
            base, f'max(max_over_time(vtorrent_process_rss_bytes[{int(args.hours)}h]))'))
        if peaks is not None:
            report.append(f'- Fleet-wide RSS peak: **{peaks / 1048576:.1f} MiB**')

        over = query(base, f'vtorrent_process_rss_bytes > {budget:.0f}', start, end, 60)
        over_by_instance = {k: len(v) for k, v in series_by_instance(over).items()}
        total_over = sum(over_by_instance.values())
        report.append(f'- Samples above budget: **{total_over}**'
                      + (f' ({over_by_instance})' if over_by_instance else ''))

        staked = series_by_instance(query(base, 'vtorrent_blocks_staked_total', start, end, 60))
        if staked:
            parts = []
            for instance, values in sorted(staked.items()):
                if not values:
                    continue
                delta = values[-1][1] - values[0][1]
                per_hour = delta / span_hours if span_hours else 0
                parts.append(f'{instance} {delta:.0f} ({per_hour:.1f}/h)')
            report.append('- Blocks staked: ' + ', '.join(parts))

        errors = series_by_instance(query(base, 'vtorrent_btc_scan_errors_total', start, end, 60))
        if errors:
            parts = []
            for instance, values in sorted(errors.items()):
                if not values:
                    continue
                parts.append(f'{instance} {values[-1][1] - values[0][1]:.0f}')
            report.append('- BTC scan errors: ' + ', '.join(parts))

        lag = single_value(query_instant(
            base, f'max(max_over_time(vtorrent_btc_scan_lag_blocks[{int(args.hours)}h]))'))
        current = query_instant(base, 'vtorrent_btc_scan_lag_blocks')
        current_max = max((float(s['value'][1]) for s in current), default=None)
        if lag is not None:
            report.append(f'- Worst BTC scan lag: **{lag:.0f} blocks**'
                          ' (a large value can only come from a full rescan, where the'
                          ' scan cursor is 0; the gauge is omitted in that state since'
                          ' the fork-height fix)')
        if current_max is not None:
            report.append(f'- Current BTC scan lag: **{current_max:.0f} blocks**')

        firing = query(base, 'ALERTS{alertstate="firing"}', start, end, 60)
        intervals = []
        for series in firing:
            name = series['metric'].get('alertname', '?')
            instance = series['metric'].get('instance', '?').split(':')[0]
            stamps = [float(t) for t, v in series.get('values', []) if float(v) == 1]
            for a, b in compress_intervals(stamps, 180):
                intervals.append((name, instance, a, b))
        report.append('')
        if intervals:
            report.append('### Alerts that fired')
            report.append('')
            for name, instance, a, b in sorted(intervals, key=lambda x: x[2]):
                report.append(f'- `{name}` on {instance}: {fmt_time(a)} → {fmt_time(b)} '
                              f'({(b - a) / 60:.0f} min)')
        else:
            report.append('### Alerts that fired')
            report.append('')
            report.append('- None in the recorded window')

    report.append('')
    report.append('### Criteria')
    report.append('')
    verdicts = []

    verdicts.append((
        'Window reached',
        span_hours >= args.hours,
        f'{span_hours:.1f} h of {args.hours:.0f} h',
    ))
    worst_peak = max((n['peak'] for n in per_node), default=0.0)
    verdicts.append((
        'RSS under budget',
        worst_peak <= args.budget_mib,
        f'peak {worst_peak:.1f} MiB against {args.budget_mib:.0f} MiB',
    ))
    restarts = max((n['restarts'] for n in per_node), default=0)
    verdicts.append(('No container restarts', restarts == 0, f'max {restarts}'))
    heights = [n['height'] for n in per_node if n['height'] is not None]
    agrees = len(set(heights)) <= 1 if heights else False
    verdicts.append((
        'Nodes agree on height',
        agrees,
        f'final heights {heights}',
    ))
    verdicts.append(('Sampler continuity', not gaps, f'{len(gaps)} gap(s)'))

    for name, ok, detail in verdicts:
        report.append(f"- {'PASS' if ok else '**FAIL**'} — **{name}** ({detail})")

    print('\n'.join(report))
    return 0 if all(ok for _, ok, _ in verdicts) else 1


if __name__ == '__main__':
    sys.exit(main())
