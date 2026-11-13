// Copies the two documents that live in the repository root into the site,
// so each has one source: docs/json.md (the --json contract) and CHANGELOG.md.
// The first heading becomes the page title. The copies are not committed.
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const site = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const pages = [
	{
		from: '../docs/json.md',
		to: 'src/content/docs/reference/json.md',
		description: 'The versioned schema of hb --json, the session commands and fork --json.',
	},
	{
		from: '../CHANGELOG.md',
		to: 'src/content/docs/reference/changelog.md',
		description: 'What changed in each hopback release.',
	},
];

for (const page of pages) {
	const source = readFileSync(resolve(site, page.from), 'utf8');
	const match = source.match(/^# (.+)\n/);
	if (!match) throw new Error(`${page.from} does not start with a "# Title" line`);
	const body = source.slice(match[0].length).trimStart();
	const front = [
		'---',
		`title: ${JSON.stringify(match[1])}`,
		`description: ${JSON.stringify(page.description)}`,
		`editUrl: https://github.com/avgvi/hopback/edit/main/${page.from.slice(3)}`,
		'---',
		'',
		`<!-- Generated from ${page.from.slice(3)} by scripts/sync-docs.mjs. Edit that file. -->`,
		'',
	].join('\n');
	const target = resolve(site, page.to);
	mkdirSync(dirname(target), { recursive: true });
	writeFileSync(target, front + body);
}
