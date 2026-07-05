// @ts-check

import cloudflare from "@astrojs/cloudflare";
import starlight from "@astrojs/starlight";
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
			sidebar: [
				{
					label: "Development",
					autogenerate: { directory: "development" },
				},
			],
		}),
	],
});
