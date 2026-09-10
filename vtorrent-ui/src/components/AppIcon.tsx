interface AppIconProps {
  size?: number
  className?: string
}

/// Original vTorrent identity: thin white VTR + VTORRENT caption on
/// espresso, sampled from the legacy vTorrent-Qt artwork.
export default function AppIcon({ size = 64, className }: AppIconProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 128 128"
      className={className}
      role="img"
      aria-label="vTorrent"
    >
      <rect width="128" height="128" rx="24" fill="#4a3131" />
      <text
        x="64"
        y="76"
        textAnchor="middle"
        fontSize="46"
        fontWeight="200"
        letterSpacing="4"
        fill="#ffffff"
        fontFamily="Inter, system-ui, sans-serif"
      >
        VTR
      </text>
      <text
        x="64"
        y="102"
        textAnchor="middle"
        fontSize="15"
        fontStyle="italic"
        fontWeight="700"
        letterSpacing="2"
        fill="#ffffff"
        fontFamily="Inter, system-ui, sans-serif"
      >
        VTORRENT
      </text>
    </svg>
  )
}
