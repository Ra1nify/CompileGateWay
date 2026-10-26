/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{js,ts,jsx,tsx}"],
  darkMode: "class",
  theme: {
    extend: {
      colors: {
        background: "#09090b", // zinc-950
        surface: "#121215", // dark surface
        card: "#18181b", // zinc-900
        cardHover: "#202024",
        border: "#27272a", // zinc-800
        borderMuted: "#3f3f46", // zinc-700
        primary: {
          DEFAULT: "#3b82f6", // blue-500
          hover: "#2563eb", // blue-600
          light: "rgba(59, 130, 246, 0.15)",
        },
        success: {
          DEFAULT: "#10b981", // emerald-500
          light: "rgba(16, 185, 129, 0.15)",
        },
        warning: {
          DEFAULT: "#f59e0b", // amber-500
          light: "rgba(245, 158, 11, 0.15)",
        },
        danger: {
          DEFAULT: "#ef4444", // red-500
          light: "rgba(239, 68, 68, 0.15)",
        },
        text: {
          DEFAULT: "#f4f4f5", // zinc-100
          secondary: "#a1a1aa", // zinc-400
          muted: "#71717a", // zinc-500
        },
      },
      fontFamily: {
        sans: [
          "-apple-system",
          "BlinkMacSystemFont",
          '"Segoe UI"',
          "Roboto",
          '"Helvetica Neue"',
          "Arial",
          "sans-serif",
        ],
        mono: [
          '"JetBrains Mono"',
          '"Cascadia Code"',
          '"Fira Code"',
          "Consolas",
          '"SF Mono"',
          "ui-monospace",
          "monospace",
        ],
      },
    },
  },
  plugins: [],
};
