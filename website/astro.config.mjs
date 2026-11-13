// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// The docs site, served by GitHub Pages at https://avgvi.github.io/hopback/.
// Logos, screenshots and the demo are read from ../docs, so the README and
// the site show the same files.
export default defineConfig({
	site: 'https://avgvi.github.io',
	base: '/hopback',
	vite: { server: { fs: { allow: ['..'] } } },
	integrations: [
		starlight({
			title: 'hopback',
			description:
				'Find, resume and fork coding-agent sessions across Claude Code, Codex and more.',
			logo: {
				dark: '../docs/brand/logo-dark.svg',
				light: '../docs/brand/logo-light.svg',
				replacesTitle: true,
			},
			favicon: '/favicon.svg',
			customCss: ['@fontsource-variable/space-grotesk', './src/styles/hopback.css'],
			social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/avgvi/hopback' }],
			editLink: { baseUrl: 'https://github.com/avgvi/hopback/edit/main/website/' },
			sidebar: [
				{
					label: 'Start here',
					items: [
						{ label: 'Install', slug: 'getting-started/install' },
						{ label: 'Quick start', slug: 'getting-started/quick-start' },
					],
				},
				{
					label: 'Guides',
					items: [
						{ label: 'Search', slug: 'guides/search' },
						{ label: 'Keys and mouse', slug: 'guides/keys' },
						{ label: 'Fork to another agent', slug: 'guides/forks' },
						{ label: 'Scripts and agents', slug: 'guides/scripting' },
					],
				},
				{
					label: 'Reference',
					items: [
						{ label: 'Supported agents', slug: 'reference/agents' },
						{ label: 'JSON output', slug: 'reference/json' },
						{ label: 'Changelog', slug: 'reference/changelog' },
					],
				},
			],
		}),
	],
});
