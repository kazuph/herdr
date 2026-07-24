# herdr task runner

# Run tests
test:
    cargo nextest run --locked --status-level fail --final-status-level fail --failure-output final --success-output never
    python3 -m unittest scripts.test_agent_detection_manifest_check scripts.test_changelog scripts.test_config_reference_check scripts.test_docs_translation_parity scripts.test_preview scripts.test_spec_contract_inventory scripts.test_vendor_libghostty_vt scripts.test_vendor_portable_pty
    python3 scripts/fork_distribution_docs_check.py
    just integration-assets-test
    just plugin-marketplace-test

# Run one nextest filter, e.g. `just test-one codex_stale_working`
test-one filter:
    cargo nextest run --locked "{{filter}}" --status-level fail --final-status-level fail --failure-output final --success-output never

# Verify fork-specific contracts after an upstream import.
fork-spec-contracts:
    # Footer, tab bar, and agent status presentation.
    just test-one non_mobile_workspace_footer
    just test-one pane_context_menu_matches_legacy_fork_order_and_separators
    just test-one same_cell_double_click_opens_legacy_pane_menu
    just test-one pane_menu_double_click_requires_same_pane_cell_within_500ms
    just test-one mouse_reporting_double_click_is_forwarded_without_opening_pane_menu
    just test-one pane_right_click_focuses_target_and_opens_legacy_menu
    just test-one tapping_active_workspace
    just test-one tapping_active_git_workspace
    just test-one releasing_active_workspace_outside
    just test-one workspace_context_menu
    just test-one worktree_context_menu
    just test-one section_new_button
    just test-one clicking_existing_section_new_button
    just test-one headless_deferred_workspace_create_preserves_requested_section
    just test-one slim_workspace_keeps_one_row
    just test-one expanded_sidebar_workspace_rows_show_state_number_and_name
    just test-one default_space_rows_render_nogit
    just test-one space_row_gap_preserves_compact_worktree_children
    just test-one packed_workspace_drag_indicator
    just test-one show_tab_bar_false_hides_multiple_tabs_but_keeps_action_bar
    just test-one state_summary_icon_animates_working_state
    just test-one onboarding_keys_persist_completion_without_opening_settings
    just test-one onboarding_click_continue_requests_completion
    just test-one startup_workspace_preserves_onboarding_mode
    just test-one github_copilot_manifest_preserves_fork_status_footer_contracts
    just test-one claude_spinner_activity_screen_fallback_is_working
    just test-one codex_working_status_immediately_before_latest_prompt_is_live
    just test-one codex_ignores_fossilized_working_text_before_latest_prompt
    just test-one frozen_claude_activity_fingerprint_expires_to_idle
    just test-one frozen_codex_working_header_expires_to_idle
    just test-one stale_codex_working_hook
    just test-one stale_screen_idle_does_not_clear_full_lifecycle_hook
    just test-one stale_activity_filter_does_not_expire_other_agents_or_untracked_working
    just test-one agent_send_writes_text_then_submits_with_enter
    just test-one agent_send_normalizes_trailing_newlines_to_one_enter
    just test-one agent_send_rejects_a_normal_shell_target
    just test-one api_pane_send_input_delivers_text_then_enter_as_separate_chunks
    just test-one pane_help_distinguishes_literal_text_from_submitted_commands

    # Exact restore identity and legacy config migration.
    just test-one session_ids_follow_the_fork_fail_closed_contract
    just test-one restore_plan_rejects_unsafe_codex_snapshot_id
    just test-one legacy_pane_restore_fields_migrate_to_native_agent_session
    just test-one legacy_pane_restore_without_session_id_stays_fail_closed
    just test-one legacy_restore_uses_saved_raw_ids_as_missing_public_pane_numbers
    just test-one native_agent_restore_defers_runtime_launch
    just test-one pending_agent_resume_deadline_uses_configured_restore_delay
    just test-one manual_restore_without_pending_resumes_distinguishes_running_agents
    just test-one legacy_report_agent_fields_feed_native_session_and_metadata
    just test-one legacy_state_report_preserves_restored_native_session
    just test-one accepted_hook_report_without_session_ref_clears_previous_ref
    just test-one clearing_hook_authority_clears_session_ref
    just test-one agent_restore_config_defaults_to_disabled
    just test-one load_live_config_accepts_agent_start_section
    just test-one load_live_config_accepts_legacy_agent_restore_section
    just test-one agent_panel_sort_config_parses_alias_and_defaults
    just test-one pane_appearance_defaults_and_parse
    just test-one api_pane_current_resolves_one
    just test-one single_pane_
    just test-one pane_scrollbar_
    just test-one tiny_pane_does_not_reserve_scrollbar_gutter
    just test-one resize_shared_runtime_resizes_background_tabs

    # Alternate binary isolation, child routing, and dispatch metadata.
    just test-one configure_from_args_sets_alternate_binary_namespace
    just test-one alternate_binary_ignores_inherited_socket_override_from_default_namespace
    just test-one alternate_binary_honors_socket_override_when_marked_explicit
    just test-one pane_base_env_marks_parent_socket_as_explicit
    just test-one model_from_cmdline_extracts_long_and_short_flags
    just test-one generated_protocol_schema_artifact_is_current
    just test-one cancelled_job_is_durable
    just test-one terminal_transition_has_single_owner
    just test-one pending_nudges_for_agent_groups_only_that_agents_queued_messages_by_room
    just test-one blocked_then_idle_flushes_multiple_pending_messages_as_direct_push
    just test-one startup_flush_walk_delivers_pending_messages_after_server_restart
    @if [ "$(uname -s)" != "Darwin" ]; then just test-one herdr_job_cancel_kills_term_ignoring_process_tree_before_marking_cancelled; fi

    # G8 update, channel, integration, download, and documentation fail-closed.
    just test-one spec_excludes_disabled_fork_commands
    just test-one integration_mutation_methods_are_not_part_of_the_public_api
    just test-one self_update_is_disabled_in_the_fork
    just test-one startup_ignores_preview_update_available_from_saved_notes
    just test-one resolve_install_source_rejects_release_download_without_explicit_binary
    python3 scripts/fork_distribution_docs_check.py
    python3 -m unittest scripts.test_spec_contract_inventory
    @! rg -n 'herdr integration (install|uninstall|status)' docs/next/website/src/content/docs

# Run fast local lint checks
lint:
    cargo fmt --check
    cargo clippy --all-targets --locked -- -D warnings

# Run PR CI checks
ci filter='all()': lint
    cargo nextest run --locked -E "{{filter}}" --no-fail-fast --status-level fail --final-status-level slow --failure-output final --success-output never
    just integration-assets-test
    just plugin-marketplace-test

# Run Windows target lint from Unix/macOS to catch cfg(windows) compile and clippy failures before CI
windows-lint:
    rustup target add x86_64-pc-windows-msvc
    LIBGHOSTTY_VT_SIMD=false cargo clippy --bin herdr --locked --target x86_64-pc-windows-msvc -- -D warnings

# Check formatting + run unit tests + Windows target lint + maintenance script tests
check: ci windows-lint
    python3 -m unittest scripts.test_agent_detection_manifest_check scripts.test_changelog scripts.test_config_reference_check scripts.test_docs_translation_parity scripts.test_preview scripts.test_spec_contract_inventory scripts.test_vendor_libghostty_vt scripts.test_vendor_portable_pty
    python3 scripts/fork_distribution_docs_check.py
    @echo "docs reminder: if this changes user-facing behavior, make sure the relevant release docs are updated or called out before release."

# Install repo-local git hooks
install-hooks:
    git config core.hooksPath .githooks
    chmod +x .githooks/pre-commit
    chmod +x .githooks/commit-msg
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

# Test bundled agent integration assets
integration-assets-test:
    bun test src/integration/assets/herdr-agent-state.test.ts

# Run plugin marketplace Worker tests
plugin-marketplace-test:
    cd workers/plugin-marketplace && bun test

# Build the vendored libghostty-vt source dist
build-libghostty-vt:
    scripts/build_vendored_libghostty_vt.sh

# Check that release docs and changelog have been finalized from docs/next before release
release-docs-check:
    python3 scripts/agent_detection_manifest_check.py --require-website
    python3 scripts/config_reference_check.py
    @if ! diff -u website/src/data/config-reference.json docs/next/website/src/data/config-reference.json; then \
        echo "error: stable config reference differs from docs/next; finalize it before releasing"; \
        exit 1; \
    fi
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
    @for file in $(find website/src/content/docs -path '*/preview' -prune -o -type f -name '*.mdx' -print); do \
        relative="${file#website/src/content/docs/}"; \
        staged="docs/next/website/src/content/docs/$relative"; \
        if [ ! -f "$staged" ]; then \
            echo "error: $staged is missing; docs/next/website/src/content/docs must mirror website/src/content/docs"; \
            exit 1; \
        fi; \
        if ! diff -u "$file" "$staged"; then \
            echo "error: $file differs from $staged; finalize website docs before releasing"; \
            exit 1; \
        fi; \
    done
    @for file in $(find docs/next/website/src/content/docs -type f -name '*.mdx' -print); do \
        relative="${file#docs/next/website/src/content/docs/}"; \
        released="website/src/content/docs/$relative"; \
        if [ ! -f "$released" ]; then \
            echo "error: $file has no matching released website doc"; \
            exit 1; \
        fi; \
    done
    @for file in website/src/content/docs/*.mdx; do \
        for locale in ja zh-cn; do \
            translated="website/src/content/docs/$locale/$(basename "$file")"; \
            if [ ! -f "$translated" ]; then \
                echo "error: $translated is missing; translate stable docs before releasing"; \
                exit 1; \
            fi; \
        done; \
    done
    @for file in website/src/content/docs/ja/*.mdx website/src/content/docs/zh-cn/*.mdx; do \
        released="website/src/content/docs/$(basename "$file")"; \
        if [ ! -f "$released" ]; then \
            echo "error: $file has no matching english doc; remove the stale translation"; \
            exit 1; \
        fi; \
    done
    python3 scripts/docs_translation_parity.py --docs-root website/src/content/docs

# Prepare the release commit without tagging or pushing (usage: just release-prepare 0.1.1)
release-prepare version:
    @printf '%s\n' '{{version}}' | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || { \
        echo "error: version must look like 0.6.6 without a v prefix"; \
        exit 1; \
    }
    @if [ -n "$(git status --porcelain)" ]; then \
        echo "error: commit your changes first"; \
        exit 1; \
    fi
    @git fetch origin master --tags
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
    @echo "v{{version}} release commit prepared. Review it, then run: just release-publish {{version}}"

# Tag and push an already-prepared release commit (usage: just release-publish 0.1.1)
release-publish version:
    @printf '%s\n' '{{version}}' | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || { \
        echo "error: version must look like 0.6.6 without a v prefix"; \
        exit 1; \
    }
    @if [ -n "$(git status --porcelain)" ]; then \
        echo "error: working tree must be clean before publishing"; \
        exit 1; \
    fi
    @branch="$(git branch --show-current)"; \
    if [ "$branch" != "master" ]; then \
        echo "error: release-publish must run from master, got $branch"; \
        exit 1; \
    fi
    @git fetch origin master --tags
    @if git rev-parse "v{{version}}" >/dev/null 2>&1; then \
        echo "error: tag v{{version}} already exists"; \
        exit 1; \
    fi
    @cargo_version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"; \
    if [ "$cargo_version" != "{{version}}" ]; then \
        echo "error: Cargo.toml version $cargo_version does not match {{version}}"; \
        exit 1; \
    fi
    just release-docs-check
    python3 scripts/changelog.py extract --version {{version}} --output /tmp/herdr-release-notes-check.md
    rm -f /tmp/herdr-release-notes-check.md
    @local_head="$(git rev-parse HEAD)"; \
    remote_head="$(git rev-parse origin/master)"; \
    if ! git merge-base --is-ancestor "$remote_head" "$local_head"; then \
        echo "error: origin/master is not an ancestor of HEAD; pull or rebase before publishing"; \
        exit 1; \
    fi; \
    if [ "$local_head" != "$remote_head" ]; then \
        echo "pushing release commit to origin/master"; \
        git push origin HEAD:master; \
    fi
    git tag -a v{{version}} -m "v{{version}}"
    git push origin v{{version}}
    @echo "v{{version}} released — GitHub Actions building binaries and updating website/latest.json"

# Prepare, verify, tag, push, and trigger the GitHub Release workflow (usage: just release 0.1.1)
release version:
    just release-prepare {{version}}
    just release-publish {{version}}

# Print default config
default-config:
    cargo run --release --locked -- --default-config
