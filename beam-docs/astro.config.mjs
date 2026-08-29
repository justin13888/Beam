// @ts-check

import cloudflare from "@astrojs/cloudflare";
import starlight from "@astrojs/starlight";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "astro/config";

// https://astro.build/config
export default defineConfig({
	adapter: cloudflare({
		platformProxy: {
			enabled: true,
		},
		imageService: "compile",
	}),
	site: "https://beam.justinchung.net",
	// Tailwind styles only the marketing pages under `src/pages/`, which reach it through
	// `src/layouts/LandingLayout.astro`. Starlight's pages never import that stylesheet, so its
	// Preflight reset cannot reach them and no `@astrojs/starlight-tailwind` shim is needed.
	vite: {
		plugins: [tailwindcss()],
	},
	integrations: [
		starlight({
			title: "Beam",
			social: [
				{
					icon: "github",
					label: "GitHub",
					href: "https://github.com/justin13888/beam",
				},
			],
			// Ordering here is pedagogical rather than alphabetical, so the sidebar is declared
			// explicitly instead of with `autogenerate`: the whole information architecture stays
			// reviewable in one place rather than scattered across per-file `sidebar.order`
			// frontmatter.
			sidebar: [
				{
					label: "Start here",
					items: [{ slug: "getting-started" }],
				},
				{
					label: "Engineering docs",
					link: "https://github.com/justin13888/beam/tree/master/docs",
					badge: { text: "Repo", variant: "note" },
					attrs: { target: "_blank", rel: "noopener" },
				},
			],
		}),
	],
});
