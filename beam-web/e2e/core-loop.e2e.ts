import { type Browser, expect, type Page, test } from "@playwright/test";

/**
 * The core loop, in the order a new operator meets it: sign in through the
 * IdP, add a library and scan it, find the scanned title by browsing and by
 * search, and direct-play it.
 *
 * Each step needs the state the one before it left behind -- a session, a
 * library, an indexed title -- so the steps run serially in one browser
 * context. A failure stops the run at the step that broke, rather than
 * reporting every later step as a second, misleading failure.
 */

/** Written into the library volume by `e2e-fixtures` in compose.e2e.yaml. */
const FIXTURE_TITLE = "Beam E2E Fixture";
const FIXTURE_FILE = "Beam E2E Fixture (2026).mp4";
/** BEAM_VIDEO_DIR inside the server container (compose.beam.yaml). */
const LIBRARY_ROOT = "/videos";
const LIBRARY_NAME = "E2E Library";

/** dex/config.yaml's static admin -- the one user in the `beam-admin` group. */
const ADMIN = { email: "admin@beam.localhost", password: "password" };

test.describe.configure({ mode: "serial" });

let page: Page;

test.beforeAll(async ({ browser }: { browser: Browser }) => {
	page = await browser.newPage();
});

test.afterAll(async () => {
	await page.close();
});

test("an admin signs in through the OIDC provider", async () => {
	await page.goto("/login");
	await page.getByRole("button", { name: "Sign in with SSO" }).click();

	// Dex with only its password database configured skips the connector
	// chooser and lands straight on its own login form.
	await expect(page).toHaveURL(/^http:\/\/dex\.beam\.localhost:5556\/dex\//);
	// By field name: Dex's template points its labels at ids the inputs do not
	// carry, so the form has no accessible names to select by.
	await page.locator('input[name="login"]').fill(ADMIN.email);
	await page.locator('input[name="password"]').fill(ADMIN.password);
	await page.getByRole("button", { name: "Login" }).click();

	// The callback lands on beam-server, which sets the session cookie and
	// redirects back into the SPA; the header then reflects GET /v1/me.
	await expect(page).toHaveURL(/^http:\/\/localhost:8080\//);
	await expect(page.getByRole("link", { name: /Hello, / })).toBeVisible();
	await expect(page.getByRole("button", { name: "Logout" })).toBeVisible();

	const me = await page.request.get("http://localhost:8000/v1/me");
	expect(me.status()).toBe(200);
	expect(await me.json()).toMatchObject({
		email: ADMIN.email,
		is_admin: true,
	});
});

test("the admin adds a library and a scan indexes the fixture", async () => {
	await page.goto("/libraries");
	await page.getByRole("button", { name: "Add Library" }).click();
	await page.getByLabel("Name").fill(LIBRARY_NAME);
	await page.getByLabel("Root Path").fill(LIBRARY_ROOT);
	await page.getByRole("button", { name: "Create", exact: true }).click();
	await expect(
		page.getByText(`Library "${LIBRARY_NAME}" created`),
	).toBeVisible();

	await page.getByTitle("Scan library").click();
	await expect(
		page.getByText(`Scan started for "${LIBRARY_NAME}"`),
	).toBeVisible();

	// The scan runs in the background; the library's file list is where its
	// result becomes visible. Reload until it does, bounded by the timeout.
	await page.getByRole("button", { name: /View Files/ }).click();
	await expect(async () => {
		await page.reload();
		await expect(page.getByText(FIXTURE_FILE)).toBeVisible({ timeout: 2_000 });
	}).toPass({ timeout: 60_000 });
});

test("the scanned title is found by browsing and by search", async () => {
	await page.goto("/explore");
	await expect(
		page.getByRole("link", { name: new RegExp(FIXTURE_TITLE) }),
	).toBeVisible();

	// A query that matches the fixture finds it...
	const search = page.getByPlaceholder("Search titles...");
	await search.fill("fixture");
	await expect(
		page.getByRole("link", { name: new RegExp(FIXTURE_TITLE) }),
	).toBeVisible();

	// ...and one that matches nothing does not, so the hit above is the
	// search's doing rather than an unfiltered listing.
	await search.fill("zzzz-no-such-title");
	await expect(
		page.getByRole("link", { name: new RegExp(FIXTURE_TITLE) }),
	).toHaveCount(0);

	await search.fill("fixture");
	await page.getByRole("link", { name: new RegExp(FIXTURE_TITLE) }).click();
	await expect(page).toHaveURL(/\/media\//);
	await expect(
		page.getByRole("heading", { name: FIXTURE_TITLE }),
	).toBeVisible();
});

test("the title direct-plays from the original file", async () => {
	const stream = page.waitForResponse(
		(response) =>
			/\/v1\/files\/[^/]+\/stream$/.test(new URL(response.url()).pathname) &&
			response.request().method() === "GET",
	);

	await page.getByRole("button", { name: "Play" }).first().click();

	// Direct play (ADR-0004): the browser fetches the original file's bytes
	// from the stream endpoint and decodes them itself.
	const response = await stream;
	expect([200, 206]).toContain(response.status());

	// Time advancing is the browser having decoded real frames, which neither
	// a 200 nor a mounted <video> element proves on its own.
	const video = page.locator("video");
	await expect
		.poll(() => video.evaluate((el: HTMLVideoElement) => el.currentTime), {
			timeout: 20_000,
		})
		.toBeGreaterThan(1);
	expect(await video.evaluate((el: HTMLVideoElement) => el.error)).toBeNull();
});
