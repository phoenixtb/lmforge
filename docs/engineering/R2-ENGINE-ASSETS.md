# Cloudflare R2 — llama.cpp CUDA engine tarballs

Engine binaries (~1 GB each) are stored on **Cloudflare R2**, served via a **custom CDN subdomain**. GitHub releases ship only the small `lmforge` CLI/UI artifacts.

**Bucket:** `lmforge-engine-assets`  
**S3 endpoint:** `https://b6d9a834cd87e72e474ed65bd4f3c2e9.r2.cloudflarestorage.com`

---

## 1. Subdomain (required)

Yes — you need a **dedicated subdomain** in Cloudflare DNS, e.g.:

| Record | Type | Target |
|--------|------|--------|
| `engines` | CNAME or R2 custom domain | R2 bucket (see below) |

**Do not** point your apex/root domain at R2. Use something like `engines.yourdomain.com`.

### Connect R2 → subdomain

1. Cloudflare dashboard → **R2** → bucket `lmforge-engine-assets`
2. **Settings** → **Custom Domains** → **Connect Domain**
3. Enter `engines.yourdomain.com` (must be a zone on the same Cloudflare account)
4. Cloudflare creates DNS + TLS automatically

Set the same URL in `scripts/llamacpp-cuda/config.env`:

```bash
export LMFORGE_ENGINE_CDN_BASE="https://engines.yourdomain.com"
```

After `update-manifest.sh`, `data/engines/llamacpp/variants-manifest.json` gets `"cdn_base": "https://engines.yourdomain.com"` and installs resolve R2 URLs.

---

## 2. Security model

End users **must** download without credentials (same as GitHub releases). Protection is **abuse control + integrity**, not secrecy.

| Layer | Setting |
|-------|---------|
| **Bucket access** | Private — no public R2.dev URL in production |
| **Public read** | Only via custom domain (Cloudflare CDN) |
| **Listing** | Disabled — only direct object URLs work |
| **Integrity** | sha256 in embedded `variants-manifest.json` |
| **Client ID** | `User-Agent: lmforge/<version>` on variant downloads |

### R2 API token (upload only)

Create token with **minimum scope**:

- Permission: **Object Read & Write**
- Scope: **this bucket only** (`lmforge-engine-assets`)

Never commit keys. Store in:

- Local: `scripts/llamacpp-cuda/config.env` (gitignored)
- CI: GitHub repository secrets (see §4)

### Cloudflare WAF / rate limiting

Dashboard → **Security** → **WAF** → custom rule on hostname `engines.yourdomain.com`:

```
(http.host eq "engines.yourdomain.com" and http.request.uri.path contains "/llamacpp/")
```

Action: **Rate limit** — e.g. **30 requests / minute / IP** on that path (enough for one install; blocks scraping).

Optional second rule: challenge if **>5 GB transferred / hour / IP** (adjust to taste).

### Cache (reduces R2 read ops + bill risk)

**Caching** → **Cache Rules** for `engines.yourdomain.com/llamacpp/*`:

- Cache eligibility: eligible
- Edge TTL: 1 month (objects are immutable versioned paths)
- Browser TTL: respect `Cache-Control` (upload script sets `max-age=31536000, immutable`)

### Billing alerts

Notifications → add alert on R2 **Class B operations** and **storage** (e.g. 2× baseline).

R2 egress through Cloudflare CDN is **free**; cost risk is mostly **read operations** if cache/WAF are misconfigured.

---

## 3. Maintainer workflow (local primary)

```bash
# One-time
cp scripts/llamacpp-cuda/config.example.env scripts/llamacpp-cuda/config.env
# Edit: R2 keys, LMFORGE_ENGINE_CDN_BASE

# Build (sequential; ~1.5–2.5 h cold on 6-core / 16 GB)
scripts/llamacpp-cuda/build-local.sh --variant all --tag b9351

# Upload + patch manifest
scripts/llamacpp-cuda/publish-r2.sh dist/llamacpp/*.tar.gz

# Rebuild lmforge so manifest is embedded
cargo build --release

# Smoke-test CDN
curl -fsSL -o /dev/null -w '%{http_code}\n' \
  "https://engines.yourdomain.com/llamacpp/b9351/lmforge-llamacpp-b9351-cuda12-linux-x64.tar.gz"

# Cut product release
git add data/engines/llamacpp/variants-manifest.json
git commit -m "..."
git tag v0.2.0 && git push origin v0.2.0
```

Object layout:

```
s3://lmforge-engine-assets/llamacpp/b9351/lmforge-llamacpp-b9351-cuda12-linux-x64.tar.gz
s3://lmforge-engine-assets/llamacpp/b9351/lmforge-llamacpp-b9351-cuda12-linux-x64.tar.gz.sha256
```

---

## 4. GitHub Actions secrets (CI fallback)

For workflow **Build llama.cpp CUDA variants** with `upload_to_r2: true`:

| Secret | Value |
|--------|-------|
| `R2_ACCESS_KEY_ID` | R2 API token access key |
| `R2_SECRET_ACCESS_KEY` | R2 API token secret |
| `R2_ENDPOINT` | `https://b6d9a834cd87e72e474ed65bd4f3c2e9.r2.cloudflarestorage.com` |
| `R2_BUCKET` | `lmforge-engine-assets` |

Add at: repo **Settings → Secrets and variables → Actions**.

**CI vs local:**

| | Local build | CI workflow |
|--|-------------|-------------|
| Build time | ~45–70 min/variant (6 cores, ccache warm ≈ minutes) | ~60 min/variant (4 vCPU) |
| Cost | Your electricity | GitHub Actions minutes |
| Manifest update | `publish-r2.sh` auto-patches | Manual — copy sha256 from job summary → `update-manifest.sh` |
| When to use | Normal releases | Emergency rebuild or no local Docker |

CI does **not** auto-commit manifest changes (avoids accidental sha/URL drift).

---

## 5. Migration from GitHub release tarballs

Until `cdn_base` is set in the manifest, installs fall back to legacy `url` (GitHub). After R2 publish + manifest update, R2 becomes primary. You can delete GitHub release `llamacpp-b9351` assets once R2 is verified — that also fixes the **Latest** tag hijack on `install-core.sh`.

---

## 6. Troubleshooting

| Symptom | Fix |
|---------|-----|
| HTTP 403 on CDN URL | Custom domain not connected; or WAF too aggressive |
| sha256 mismatch after upload | Rebuilt tarball without updating manifest — rerun `update-manifest.sh` + rebuild `lmforge` |
| `YOURDOMAIN` in cdn_base | Replace in `config.env`; placeholder is ignored, falls back to GitHub `url` |
| Docker OOM during build | `LMFORGE_BUILD_JOBS=4` (default in build-local.sh) |
