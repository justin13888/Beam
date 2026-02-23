import { defineConfig } from "vitest/config";

export default defineConfig({
	test: {
		environment: "jsdom",
		passWithNoTests: true,
		coverage: {
			provider: "v8",
			reporter: ["text", "lcov", "html"],
			reportsDirectory: "coverage",
			// TODO: Enforce 80% thresholds once test coverage reaches that level:
			// thresholds: {
			//   lines: 80,
			//   functions: 80,
			//   branches: 80,
			//   statements: 80,
			// },
		},
	},
});
