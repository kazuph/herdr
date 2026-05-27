# herdr task runner

# Run tests
test:
    cargo nextest run --locked --status-level fail --final-status-level fail --failure-output final --success-output never
    python3 -m unittest scripts.test_changelog scripts.test_vendor_libghostty_vt

# Run fast local lint checks
lint:
    cargo fmt --check
    cargo clippy --all-targets --locked -- -D warnings

# Run PR CI checks
ci: lint
    cargo nextest run --locked --status-level fail --final-status-level fail --failure-output final --success-output never

# Check formatting + run unit tests + maintenance script tests
check: ci
    python3 -m unittest scripts.test_changelog scripts.test_vendor_libghostty_vt
    @echo "docs reminder: if this changes user-facing behavior, make sure the relevant release docs are updated or called out before release."

# Install repo-local git hooks
install-hooks:
    git config core.hooksPath .githooks
    chmod +x .githooks/pre-commit
    @echo "installed git hooks from .githooks"

# Build release binary
build:
    cargo build --release --locked

# Build, ad-hoc sign, and install to ~/.local/bin/herdr (macOS dev workflow)
install-local: build
    #!/usr/bin/env bash
    set -euo pipefail
    bin="target/release/herdr"
    dest="${HERDR_INSTALL_DIR:-$HOME/.local/bin}/herdr"
    if [[ "$(uname -s)" == Darwin ]]; then
        codesign -s - -f "$bin"
    fi
    install -m 755 "$bin" "$dest"
    if [[ "$(uname -s)" == Darwin ]]; then
        codesign -s - -f "$dest"
    fi
    echo "installed $dest"

# Build the website and documentation
website-build:
    cd website && bun install --frozen-lockfile && bun run build

# Build the vendored libghostty-vt source dist
build-libghostty-vt:
    scripts/build_vendored_libghostty_vt.sh

# Check that release docs and changelog have been finalized from docs/next before release
release-docs-check:
    @for file in README.md CHANGELOG.md; do \
        if ! diff -u "$file" "docs/next/$file"; then \
            echo "error: $file differs from docs/next/$file; finalize release docs before releasing"; \
            exit 1; \
        fi; \
    done
    @for file in CONFIGURATION.md INTEGRATIONS.md SOCKET_API.md; do \
        if [ -e "$file" ]; then \
            echo "error: $file was replaced by website docs; remove the root copy"; \
            exit 1; \
        fi; \
    done
    @test -d docs/next/website/src/content/docs
    @for file in website/src/content/docs/*.mdx; do \
        staged="docs/next/website/src/content/docs/$(basename "$file")"; \
        if [ ! -f "$staged" ]; then \
            echo "error: $staged is missing; docs/next/website/src/content/docs must mirror website/src/content/docs"; \
            exit 1; \
        fi; \
        if ! diff -u "$file" "$staged"; then \
            echo "error: $file differs from $staged; finalize website docs before releasing"; \
            exit 1; \
        fi; \
    done
    @for file in docs/next/website/src/content/docs/*.mdx; do \
        released="website/src/content/docs/$(basename "$file")"; \
        if [ ! -f "$released" ]; then \
            echo "error: $file has no matching released website doc"; \
            exit 1; \
        fi; \
    done

# Finalize changelog, bump version, commit, tag, push, and trigger the GitHub Release workflow (usage: just release 0.1.1)
release version:
    @if [ -n "$(git status --porcelain)" ]; then \
        echo "error: commit your changes first"; \
        exit 1; \
    fi
    @if git rev-parse "v{{version}}" >/dev/null 2>&1; then \
        echo "error: tag v{{version}} already exists"; \
        exit 1; \
    fi
    just release-docs-check
    python3 scripts/changelog.py prepare --version {{version}}
    cp CHANGELOG.md docs/next/CHANGELOG.md
    sed -i.bak 's/^version = ".*"/version = "{{version}}"/' Cargo.toml && rm -f Cargo.toml.bak
    cargo update -p herdr --offline
    just check
    git add CHANGELOG.md docs/next/CHANGELOG.md Cargo.toml Cargo.lock
    git diff --cached --quiet || git commit -m "release: v{{version}}"
    git tag -a v{{version}} -m "v{{version}}"
    git push --follow-tags
    @echo "v{{version}} released — GitHub Actions building binaries and updating website/latest.json"

# Print default config
default-config:
    cargo run --release --locked -- --default-config
