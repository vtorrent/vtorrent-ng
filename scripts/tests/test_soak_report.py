import json
import pathlib
import subprocess
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / 'soak-report.py'


class SoakReportTests(unittest.TestCase):
    def test_missing_node_and_telemetry_cannot_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / 'samples.jsonl'
            rows = [
                {'timestamp': f'2026-01-01T00:00:0{i}+00:00',
                 'nodes': [{'info': {'block_height': 5}}]}
                for i in (0, 4)
            ]
            path.write_text(''.join(json.dumps(row) + '\n' for row in rows))
            result = subprocess.run(
                ['python3', str(SCRIPT), '--samples', str(path), '--hours', '0.001'],
                capture_output=True, text=True,
            )
            self.assertNotEqual(result.returncode, 0, result.stdout)


if __name__ == '__main__':
    unittest.main()
