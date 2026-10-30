const fs = require('node:fs');
const path = require('node:path');

module.exports = async ({ github, context, core }) => {
  const cargo = fs.readFileSync('Cargo.toml', 'utf8');
  const version = cargo.match(/^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"/m)?.[1];
  if (!version) throw new Error('No stable package version found');
  const tag = `v${version}`;
  const repository = context.repo;
  let existing;
  try {
    existing = (await github.rest.repos.getReleaseByTag({ ...repository, tag })).data;
  } catch (error) {
    if (error.status !== 404) throw error;
  }
  if (existing && !existing.draft) {
    core.notice(`${tag} is already published; historical releases are left unchanged.`);
    return;
  }
  const notes = fs.readFileSync(`docs/releases/${tag}.md`, 'utf8').trim();
  if (notes.length < 100) throw new Error('Write readable release notes before publishing');
  const names = [`SubscriptionsReminder-v${version}.exe`, 'SHA256SUMS.txt'];
  for (const name of names) {
    if (!fs.statSync(path.join('dist', name)).isFile()) throw new Error(`Missing asset: ${name}`);
  }
  if (existing && existing.target_commitish !== context.sha) {
    throw new Error('A draft exists for a different commit. Review it before publishing.');
  }
  const release = existing ?? (await github.rest.repos.createRelease({
    ...repository, tag_name: tag, target_commitish: context.sha,
    name: `Subscriptions Reminder ${tag}`, body: notes, draft: true, prerelease: false,
  })).data;
  for (const name of names) {
    if (release.assets.some(asset => asset.name === name)) continue;
    await github.rest.repos.uploadReleaseAsset({
      ...repository, release_id: release.id, name,
      data: fs.readFileSync(path.join('dist', name)),
      headers: { 'content-type': name.endsWith('.exe') ? 'application/octet-stream' : 'text/plain' },
    });
  }
  await github.rest.repos.updateRelease({ ...repository, release_id: release.id, draft: false, body: notes });
  core.notice(`Published ${tag} with the Windows x64 executable and SHA-256 checksums.`);
};
