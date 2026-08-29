import { fileURLToPath, URL } from "node:url";
import { tanstackRouter } from "@tanstack/router-plugin/vite";
import viteReact from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

export default defineConfig({
	// The router plugin regenerates `routeTree.gen.ts` from the files in
	// `src/routes`. It runs here as well as in `vite.config.ts` so the tree the
	// tests mount is the one the route files describe -- without it a renamed
	// or added route silently keeps testing the previous tree.
	plugins: [
		tanstackRouter({ target: "react", autoCodeSplitting: true }),
		viteReact(),
	],
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
			// Calibrated against a measured baseline of ~82.6% lines / ~73.9%
			// branches / ~75.4% functions / ~79.1% statements, taken once the
			// suite stopped mocking the router and the API client away and
			// started driving a real memory router with MSW at the network
			// boundary (see `src/test/harness.tsx`). Thresholds sit ~3 points
			// under the measured numbers so unrelated diffs don't flap the
			// gate. Ratchet up over time, don't relax to pass a PR.
			thresholds: {
				lines: 79,
				functions: 72,
				branches: 70,
				statements: 76,
			},
		},
	},
});
