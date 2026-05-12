// Uploads release artifacts + install.sh + latest pointer to Vercel Blob.
//
// Reads:
//   process.env.VERSION   — e.g. "0.0.6"
//   process.env.BLOB_READ_WRITE_TOKEN
// Reads files:
//   dist/cc-ledger-*.tar.gz  (4 tarballs from the build matrix)
//   dist/SHA256SUMS
//   install.sh
//
// Order matters: `latest` is written LAST so the version flips atomically
// only after every artifact is in place.

import { readdir, readFile } from 'node:fs/promises'
import { put } from '@vercel/blob'

const VERSION = process.env.VERSION
if (!VERSION) {
  console.error('VERSION env var is required')
  process.exit(1)
}

async function upload(pathname, body, contentType) {
  const res = await put(pathname, body, {
    access: 'public',
    addRandomSuffix: false,
    allowOverwrite: true,
    contentType,
  })
  console.log(`  ${pathname} -> ${res.url}`)
  return res
}

const distFiles = await readdir('dist')
const tarballs = distFiles.filter((f) => f.endsWith('.tar.gz')).sort()
if (tarballs.length === 0) {
  console.error('no tarballs found in dist/')
  process.exit(1)
}

console.log(`Uploading ${tarballs.length} tarballs for version ${VERSION}…`)
for (const name of tarballs) {
  const body = await readFile(`dist/${name}`)
  await upload(`versions/${VERSION}/${name}`, body, 'application/gzip')
}

console.log('Uploading SHA256SUMS…')
const sums = await readFile('dist/SHA256SUMS')
await upload(`versions/${VERSION}/SHA256SUMS`, sums, 'text/plain')

console.log('Uploading install.sh…')
const installer = await readFile('install.sh')
await upload('install.sh', installer, 'text/x-shellscript')

console.log('Flipping latest pointer…')
await upload('latest', VERSION, 'text/plain')

console.log(`Done. cc-ledger ${VERSION} is live.`)
