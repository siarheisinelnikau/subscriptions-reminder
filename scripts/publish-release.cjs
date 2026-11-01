const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');

module.exports = async ({ github, context, core }) => {
  const cargo = fs.readFileSync('Cargo.toml', 'utf8');
  const version = cargo.match(/^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"/m)?.[1];
  if (!version) throw new Error('No stable package version found');
  const tag = `v${version}`;
  if (context.eventName === 'push') {
    const subject = context.payload.head_commit?.message.split('\n')[0].replace(/\r$/, '');
    if (context.ref !== 'refs/heads/main' || subject !== `Release localization ${tag}`) {
      throw new Error('The exact final release subject must match the Cargo version.');
    }
  }
  try {
    const {data: reference} = await github.rest.git.getRef({ ...context.repo, ref: `tags/${tag}` });
    if (reference.object.type !== 'commit' || reference.object.sha !== context.sha) {
      throw new Error('An existing version tag belongs to another commit.');
    }
  } catch (error) { if (error.status !== 404) throw error; }
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
  const executable = names[0];
  const executableData = fs.readFileSync(path.join('dist', executable));
  const checksum = crypto.createHash('sha256').update(executableData).digest('hex');
  if (fs.readFileSync('dist/SHA256SUMS.txt', 'utf8').trim() !== `${checksum}  ${executable}`) {
    throw new Error('The packaged checksum does not match the executable.');
  }
  if (existing && existing.target_commitish !== context.sha) {
    throw new Error('A draft exists for a different commit. Review it before publishing.');
  }
  const release = existing ?? (await github.rest.repos.createRelease({
    ...repository, tag_name: tag, target_commitish: context.sha,
    name: `Subscriptions Reminder ${tag}`, body: notes, draft: true, prerelease: false,
  })).data;
  for (const name of names) {
    const data = fs.readFileSync(path.join('dist', name));
    const digest = 'sha256:' + crypto.createHash('sha256').update(data).digest('hex');
    const prior = release.assets.find(asset => asset.name === name);
    if (prior) {
      if (prior.size !== data.length || prior.digest !== digest) throw new Error(`Existing draft asset differs: ${name}`);
      continue;
    }
    const uploaded = (await github.rest.repos.uploadReleaseAsset({
      ...repository, release_id: release.id, name, data,
      headers: { 'content-type': name.endsWith('.exe') ? 'application/octet-stream' : 'text/plain' },
    })).data;
    if (uploaded.size !== data.length || uploaded.digest !== digest) throw new Error(`Uploaded asset digest differs: ${name}`);
  }
  const ready = (await github.rest.repos.getRelease({ ...repository, release_id: release.id })).data;
  if (ready.assets.length !== names.length) throw new Error('Unexpected or missing release assets.');
  for (const name of names) {
    const data = fs.readFileSync(path.join('dist', name));
    const uploaded = ready.assets.find(asset => asset.name === name);
    if (!uploaded || uploaded.size !== data.length || uploaded.digest !== 'sha256:' + crypto.createHash('sha256').update(data).digest('hex')) {
      throw new Error(`The release remains a draft because an upload is incomplete: ${name}`);
    }
  }
  await github.rest.repos.updateRelease({ ...repository, release_id: release.id, draft: false, body: notes });
  core.notice(`Published ${tag} with the Windows x64 executable and SHA-256 checksums.`);
};
