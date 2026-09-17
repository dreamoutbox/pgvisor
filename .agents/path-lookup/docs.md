### If you want to modify the documentation website or GitHub Pages deployment CI, then check:

- `docs/astro.config.ts` = site configuration, base path resolution, branding metadata, and Nimbus integration rules
- `docs/src/pages/index.astro` = homepage hero, guide cards, and feature overview
- `docs/src/content/docs/quick-start.mdx` = quick start guide and bootstrap walkthrough
- `docs/src/content/docs/architecture.mdx` = system architecture, proxy mechanics, and consensus specs
- `docs/src/content/docs/web-dashboard.mdx` = web UI overview, switchover guide, and guarded SQL console
- `docs/src/content/docs/adding-new-node.mdx` = horizontal scaling and replica bootstrapping guide
- `docs/src/content/docs/configure-backup.mdx` = OpenDAL storage config, environment vars, and retention schedules
- `docs/src/content/docs/backup.mdx` = continuous WAL streaming and basebackup snapshot mechanics
- `docs/src/content/docs/restore.mdx` = PITR procedures, timeline rewinds, and standby re-sync workflows
- `docs/src/content/docs/tls-ssl.mdx` = client/proxy/backend TLS encryption, dev cert generation, and mTLS
- `docs/src/components/Mermaid.astro` = Mermaid diagram component and auto-rendering integration
- `docs/src/components/ui/steps/Steps.astro` = step list component styles, marker counters, and nested list handling
- `docs/package.json` = documentation dependencies, scripts, and package manager specification
- `.github/workflows/docs.yml` = GitHub Actions workflow dispatch pipeline for building and deploying to GitHub Pages
