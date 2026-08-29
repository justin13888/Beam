// @ts-check

import cloudflare from "@astrojs/cloudflare";
import starlight from "@astrojs/starlight";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig } from "astro/config";
import { ogImageUrl, site } from "./src/site";

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
			// src/pages/404.astro serves this instead. Both routes are entirely static segments,
			// so leaving Starlight's injected 404 in place is a real collision rather than a
			// silent overwrite -- Astro warns today and says it becomes a hard error in a future
			// release, which would break docs:build and the release deploy.
			disable404Route: true,
			customCss: ["./src/styles/starlight.css"],
			// Starlight derives <title> and the meta description from each page's frontmatter;
			// these are the site-wide tags it does not emit. They are plain descriptor objects
			// rather than a component, which is why og:image must be an absolute URL to an
			// unhashed file in public/ -- see src/site.ts.
			head: [
				{
					tag: "meta",
					attrs: { property: "og:site_name", content: site.name },
				},
				{ tag: "meta", attrs: { property: "og:type", content: "website" } },
				{ tag: "meta", attrs: { property: "og:image", content: ogImageUrl } },
				{
					tag: "meta",
					attrs: { name: "twitter:card", content: "summary_large_image" },
				},
				{ tag: "meta", attrs: { name: "twitter:image", content: ogImageUrl } },
				{ tag: "meta", attrs: { name: "theme-color", content: "#0b1220" } },
			],
			favicon: "/favicon.svg",
			// Starlight appends the entry's filePath, which is relative to the Astro project
			// root, so the base URL has to reach beam-docs/ rather than the repo root.
			editLink: {
				baseUrl: "https://github.com/justin13888/beam/edit/master/beam-docs/",
			},
			// Read from git history, so CI and the release deploy must check out full history --
			// a shallow clone silently renders wrong dates rather than failing the build.
			lastUpdated: true,
			// Two variants because Starlight renders the logo as an <img>: the SVG cannot inherit
			// the page's colour, so each theme needs its own baked-in stroke. `alt` is empty
			// because the site title sits beside it and would otherwise be announced twice.
			logo: {
				dark: "./src/assets/beam-mark.svg",
				light: "./src/assets/beam-mark-light.svg",
				alt: "",
			},
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
					items: [
						{ slug: "getting-started" },
						{ slug: "install" },
						{ slug: "first-run", label: "Your first library" },
					],
				},
				{
					label: "Using Beam",
					items: [
						{ slug: "using/signing-in" },
						{ slug: "using/browsing-and-search", label: "Browsing and search" },
						{ slug: "using/watching" },
						{
							slug: "using/playback-compatibility",
							label: "When a file won't play",
						},
						{ slug: "using/downloads", label: "Downloading" },
						{ slug: "using/continue-watching", label: "Resume and history" },
						{
							slug: "using/account-and-sessions",
							label: "Account and sessions",
						},
					],
				},
				{
					label: "Running Beam",
					items: [
						{ slug: "operate/libraries" },
						{ slug: "operate/identity-and-access" },
						{ slug: "operate/metadata", label: "Metadata and artwork" },
						{ slug: "operate/monitoring", label: "Monitoring and logs" },
						{ slug: "operate/backup-and-upgrade" },
						{ slug: "operate/production" },
					],
				},
				{
					label: "Reference",
					items: [
						{ slug: "reference/configuration" },
						{
							slug: "reference/errors",
							badge: { text: "Unstable", variant: "caution" },
						},
						{ slug: "reference/api" },
					],
				},
				{
					label: "Help",
					items: [
						{ slug: "help/troubleshooting" },
						{ slug: "help/faq", label: "FAQ" },
						{ slug: "help/support", label: "Getting help" },
					],
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
