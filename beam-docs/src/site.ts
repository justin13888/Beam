/**
 * Site-wide constants shared by the marketing pages and Starlight.
 *
 * These values have to appear in two places that cannot share a component: `Seo.astro` renders
 * the head for `src/pages/*`, while Starlight's pages are served by its own layout and take
 * extra tags through the `head` array in `astro.config.mjs`. Defining them here keeps the two
 * from drifting.
 */
export const site = {
	name: "Beam",
	/** Must match `site` in astro.config.mjs; used to absolutise URLs for crawlers. */
	origin: "https://beam.justinchung.net",
	description:
		"Beam is a self-hosted media server for home labs and small teams. It streams your existing files to the browser without transcoding them.",
	repo: "https://github.com/justin13888/beam",
	/**
	 * Lives in `public/` rather than `src/assets/` on purpose. Astro content-hashes assets it
	 * processes, and Starlight's `head` entries are static tag descriptors that cannot reference
	 * the hashed name. Crawlers also want an absolute URL here, not a path.
	 */
	ogImage: "/og/beam.png",
} as const;

export const ogImageUrl = new URL(site.ogImage, site.origin).href;
