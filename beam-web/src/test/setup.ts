import "@testing-library/jest-dom/vitest";
import { afterAll, afterEach, beforeAll } from "vitest";
import { server } from "./server";

beforeAll(() => server.listen({ onUnhandledRequest: "error" }));
// No localStorage/sessionStorage reset here: auth is cookie-only and nothing
// in beam-web writes web storage. (On Node >= 26 the bare `localStorage`
// global is Node's own experimental, undefined-without-a-flag one -- jsdom's
// gets shadowed -- so touching it here also breaks every test.)
afterEach(() => {
	server.resetHandlers();
});
afterAll(() => server.close());
