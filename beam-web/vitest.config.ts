import { fileURLToPath, URL } from "node:url";
import viteReact from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

export default defineConfig({
	plugins: [viteReact()],
	resolve: {
		alias: {
			"@": fileURLToPath(new URL("./src", import.meta.url)),
		},
	},
	test: {
		environment: "jsdom",
		globals: true,
		setupFiles: ["./src/test/setup.ts"],
		passWithNoTests: true,
		env: {
			C_STREAM_SERVER_URL: "http://localhost:8000",
		},
		coverage: {
			provider: "v8",
			reporter: ["text", "lcov", "html"],
			reportsDirectory: "coverage",
			// `include` alone makes every matching source file count in the
			// denominator, not just ones a test happens to import -- without
			// it the number silently only reflects the files under test,
			// which is meaningless as a gate (see docs/testing.md).
			include: ["src/**/*.{ts,tsx}"],
			exclude: [
				"src/**/*.test.{ts,tsx}",
				"src/**/*.gen.ts",
				"src/routeTree.gen.ts",
				"src/main.tsx",
				"src/test/**",
			],
			// Calibrated against a measured honest baseline of ~68.6% lines /
			// ~59.4% branches / ~57.6% functions / ~65.6% statements, once the
			// route components (explore, admin, history, profile, libraries,
			// media detail, library detail) gained subcutaneous component
			// tests behind router/query harnesses. Thresholds sit ~3 points
			// under the measured numbers so unrelated diffs don't flap the
			// gate. Ratchet up over time, don't relax to pass a PR.
			thresholds: {
				lines: 65,
				functions: 54,
				branches: 56,
				statements: 62,
			},
		},
	},
});
