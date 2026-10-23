import { defineConfig, devices } from "@playwright/test";

/**
 * The browser end-to-end suite (#74): a real Chromium driving the real SPA
 * against the composed Postgres, Dex and beam-server stack.
 *
 * Run it through `mise run ts:e2e`, which brings that stack up first and tears
 * it down afterwards. Playwright itself only serves the SPA (`webServer`
 * below); it never starts the backend, so a bare `playwright test` with no
 * stack fails at the first request rather than testing something else.
 *
 * Both origins are fixed rather than configurable. Dex's static client
 * registers exactly `http://localhost:8000/v1/auth/callback`
 * (dex/config.yaml), and beam-server's post-login redirect and CSRF
 * allowed-origin set are both its `BEAM_WEB_URL`, which `ts:e2e` pins to the
 * web origin here.
 */
const WEB_ORIGIN = "http://localhost:8080";
const SERVER_ORIGIN = "http://localhost:8000";

export default defineConfig({
	testDir: "./e2e",
	// `.e2e.ts`, not `.spec.ts`: vitest's default include matches `*.spec.ts`,
	// and these must never run under jsdom as part of `ts:test`.
	testMatch: "**/*.e2e.ts",
	outputDir: "./e2e/.results",
	// One worker over one stack. The specs share a server, a database and a
	// library, so running them in parallel would make each one's assertions
	// depend on the others' timing.
	workers: 1,
	fullyParallel: false,
	forbidOnly: !!process.env.CI,
	// No retries: a step that passes on the second attempt is a real ordering
	// or readiness bug, and retrying would hide exactly that.
	retries: 0,
	reporter: process.env.CI
		? [["list"], ["html", { open: "never", outputFolder: "./e2e/.report" }]]
		: [["list"]],
	timeout: 60_000,
	expect: { timeout: 15_000 },
	use: {
		baseURL: WEB_ORIGIN,
		trace: "retain-on-failure",
		screenshot: "only-on-failure",
	},
	projects: [
		{
			name: "chromium",
			use: { ...devices["Desktop Chrome"] },
		},
	],
	webServer: {
		// `vite build` alone, not `bun run build`: that script also runs `tsc`,
		// which cannot pass until `api.gen.ts` is regenerated (#146). Vite never
		// type-checks, so the bundle it emits is the one the image would serve.
		command: "vite build && vite preview --port 8080 --strictPort",
		url: WEB_ORIGIN,
		reuseExistingServer: false,
		timeout: 180_000,
		env: {
			C_STREAM_SERVER_URL: SERVER_ORIGIN,
		},
	},
});
