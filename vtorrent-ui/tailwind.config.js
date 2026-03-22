/** @type {import('tailwindcss').Config} */
export default {
  content: ['./index.html', './src/**/*.{js,ts,jsx,tsx}'],
  theme: {
    extend: {
      colors: {
        // vTorrent brand colors - deep navy + electric teal
        vtorrent: {
          50:  '#edfcf9',
          100: '#d2f7f1',
          200: '#a9ede4',
          300: '#72ddd3',
          400: '#3ec5bc',
          500: '#25a9a2',  // primary brand teal
          600: '#1d8880',
          700: '#1c6d68',
          800: '#1c5754',
          900: '#1b4845',
          950: '#0a2e2c',
        },
        navy: {
          800: '#0f1923',
          900: '#0a1118',
          950: '#060c12',
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
