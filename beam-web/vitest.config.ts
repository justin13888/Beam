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
			// which is meaningless as a gate (see docs/testing/coverage.md).
			include: ["src/**/*.{ts,tsx}"],
			exclude: [
				"src/**/*.test.{ts,tsx}",
				"src/**/*.gen.ts",
				"src/routeTree.gen.ts",
				"src/main.tsx",
				"src/test/**",
			],
			// Calibrated against a measured honest baseline of ~13.8% lines /
			// ~4.7% branches / ~13.3% functions / ~13.8%
			// statements -- most of `beam-web`'s current tests cover pure
			// hooks/utils (already ~98%+), not the large route components,
			// which need router/query harness investment to test
			// meaningfully. Ratchet up over time, don't relax to pass a PR.
			thresholds: {
				lines: 12,
				functions: 10,
				branches: 3,
				statements: 12,
			},
		},
	},
});
