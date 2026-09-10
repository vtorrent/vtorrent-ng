interface AppIconProps {
  size?: number
  className?: string
}

/// Hex-monogram VTR mark (teal ring, transparent core). Replaces the old
/// "VT" text badge in the welcome hero and sidebar.
export default function AppIcon({ size = 64, className }: AppIconProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 128 128"
      className={className}
      role="img"
      aria-label="vTorrent-NG"
    >
      <defs>
        <linearGradient id="vtr-hex" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor="#3ec5bc" />
          <stop offset="1" stopColor="#1d8880" />
        </linearGradient>
      </defs>
      <polygon
        points="64,6 116,35 116,93 64,122 12,93 12,35"
        fill="none"
        stroke="url(#vtr-hex)"
        strokeWidth="9"
      />
      <text
        x="64"
        y="82"
        textAnchor="middle"
        fontSize="36"
        fontWeight="700"
        fill="#3ec5bc"
        fontFamily="Inter, system-ui, sans-serif"
      >
        VTR
      </text>
    </svg>
  )
}
