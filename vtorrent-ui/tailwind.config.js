/** @type {import('tailwindcss').Config} */
export default {
  content: ['./index.html', './src/**/*.{js,ts,jsx,tsx}'],
  theme: {
    extend: {
      colors: {
        // Theme-aware brand colors. Triplets are set per theme in index.css
        // (:root = modern teal/navy, [data-theme='legacy'] = espresso/taupe).
        // rgb() with <alpha-value> keeps /opacity modifiers working.
        vtorrent: {
          50:  'rgb(var(--vt-50) / <alpha-value>)',
          100: 'rgb(var(--vt-100) / <alpha-value>)',
          200: 'rgb(var(--vt-200) / <alpha-value>)',
          300: 'rgb(var(--vt-300) / <alpha-value>)',
          400: 'rgb(var(--vt-400) / <alpha-value>)',
          500: 'rgb(var(--vt-500) / <alpha-value>)',  // primary brand
          600: 'rgb(var(--vt-600) / <alpha-value>)',
          700: 'rgb(var(--vt-700) / <alpha-value>)',
          800: 'rgb(var(--vt-800) / <alpha-value>)',
          900: 'rgb(var(--vt-900) / <alpha-value>)',
          950: 'rgb(var(--vt-950) / <alpha-value>)',
        },
        navy: {
          700: 'rgb(var(--nv-700) / <alpha-value>)',
          800: 'rgb(var(--nv-800) / <alpha-value>)',
          900: 'rgb(var(--nv-900) / <alpha-value>)',
          950: 'rgb(var(--nv-950) / <alpha-value>)',
        }
      },
      fontFamily: {
        sans: ['Inter', 'system-ui', 'sans-serif'],
        mono: ['JetBrains Mono', 'Fira Code', 'monospace'],
      },
      animation: {
        'pulse-slow': 'pulse 3s cubic-bezier(0.4, 0, 0.6, 1) infinite',
        'spin-slow': 'spin 3s linear infinite',
      }
    },
  },
  plugins: [],
}
