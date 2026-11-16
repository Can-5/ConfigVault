<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { open, save } from '@tauri-apps/plugin-dialog';
  import { cycleTheme, getPref } from '$lib/theme';
  import { countActions, formatBytes, formatDate, shortPath } from '$lib/util';

  interface BackupMeta { id: string; name: string; created_at: number; sources: string[]; total_bytes: number; file_count: number }
  interface ManifestEntry { target: string; archive: string; size: number; sha256: string; mode: number }
  interface Manifest { id: string; name: string; created_at: number; sources: string[]; excludes: string[]; entries: ManifestEntry[]; total_bytes: number }
  interface RestoreAction { kind: 'create' | 'overwrite' | 'skip'; target: string; size?: number; reason?: string }
  interface RestoreReport { dry_run: boolean; actions: RestoreAction[]; errors: string[]; verified: number; failed_verify: string[]; backup_id: string | null }

  let backups = $state<BackupMeta[]>([]);
  let selectedId = $state<string | null>(null);
  let details = $state<Manifest | null>(null);
  let busy = $state(false);
  let toast = $state<string | null>(null);
  let toastTimer: ReturnType<typeof setTimeout> | undefined;

  // New-snapshot modal
  let newOpen = $state(false);
  let snapName = $state('');
  let snapSources = $state<string[]>([]);
  let snapExcludes = $state('node_modules\n.git\ntarget\ndist\n*.log');
  let creating = $state(false);

  // Restore preview modal
  let preview = $state<RestoreReport | null>(null);
  let previewFor = $state<string | null>(null);
  let restoring = $state(false);

  // Two-step delete
  let deleteArmed = $state(false);
  let deleteTimer: ReturnType<typeof setTimeout> | undefined;

  function showToast(msg: string) {
    toast = msg;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (toast = null), 2800);
  }

  async function refresh() {
    backups = await invoke<BackupMeta[]>('list_backups');
    if (selectedId && !backups.some((b) => b.id === selectedId)) {
      selectedId = null;
      details = null;
    }
    if (!selectedId && backups.length > 0) await select(backups[0].id);
  }

  async function select(id: string) {
    selectedId = id;
    deleteArmed = false;
    try {
      details = await invoke<Manifest>('backup_details', { id });
    } catch {
      details = null;
    }
  }

  async function importBackup() {
    const picked = await open({ multiple: false, filters: [{ name: 'ConfigVault backup', extensions: ['tar.gz', 'gz'] }] });
    if (typeof picked !== 'string') return;
    try {
      const meta = await invoke<BackupMeta>('import_backup', { src: picked });
      showToast(`Imported “${meta.name}”`);
      await refresh();
      await select(meta.id);
    } catch (e) {
      showToast(`Import failed: ${e}`);
    }
  }

  async function addFolders() {
    const picked = await open({ directory: true, multiple: true });
    const list = Array.isArray(picked) ? picked : picked ? [picked] : [];
    const merged = new Set([...snapSources, ...list]);
    snapSources = [...merged];
  }

  async function addFiles() {
    const picked = await open({ multiple: true });
    const list = Array.isArray(picked) ? picked : picked ? [picked] : [];
    const merged = new Set([...snapSources, ...list]);
    snapSources = [...merged];
  }

  async function createSnapshot() {
    if (snapSources.length === 0 || creating) return;
    creating = true;
    try {
      const meta = await invoke<BackupMeta>('create_backup', {
        name: snapName,
        sources: snapSources,
        excludes: snapExcludes.split('\n').map((s) => s.trim()).filter(Boolean),
      });
      showToast(`Snapshot “${meta.name}” saved (${meta.file_count} files)`);
      newOpen = false;
      snapName = '';
      snapSources = [];
      await refresh();
      await select(meta.id);
    } catch (e) {
      showToast(`Backup failed: ${e}`);
    } finally {
      creating = false;
    }
  }

  async function openRestorePreview() {
    if (!selectedId) return;
    previewFor = selectedId;
    try {
      preview = await invoke<RestoreReport>('restore_preview', { id: selectedId });
    } catch (e) {
      showToast(`Preview failed: ${e}`);
    }
  }

  async function confirmRestore() {
    if (!previewFor || restoring) return;
    restoring = true;
    try {
      const report = await invoke<RestoreReport>('restore_backup', { id: previewFor });
      const parts = [`Restored ${report.verified} file(s)`];
      if (report.backup_id) parts.push('pre-restore backup created');
      if (report.failed_verify.length > 0) parts.push(`${report.failed_verify.length} failed verification`);
      if (report.errors.length > 0) parts.push(`${report.errors.length} skipped`);
      showToast(parts.join(' · '));
      preview = null;
      previewFor = null;
      await refresh();
    } catch (e) {
      showToast(`Restore failed: ${e}`);
    } finally {
      restoring = false;
    }
  }

  async function exportBackup() {
    if (!selectedId) return;
    const current = backups.find((b) => b.id === selectedId);
    const dest = await save({
      defaultPath: `${(current?.name ?? 'configvault-backup').replace(/[^\w.-]+/g, '-')}.tar.gz`,
      filters: [{ name: 'ConfigVault backup', extensions: ['tar.gz'] }],
    });
    if (!dest) return;
    try {
      await invoke('export_backup', { id: selectedId, dest });
      showToast('Backup exported');
    } catch (e) {
      showToast(`Export failed: ${e}`);
    }
  }

  function armDelete() {
    deleteArmed = true;
    clearTimeout(deleteTimer);
    deleteTimer = setTimeout(() => (deleteArmed = false), 3200);
  }

  async function confirmDelete() {
    if (!selectedId) return;
    try {
      await invoke('delete_backup', { id: selectedId });
      showToast('Backup deleted');
      selectedId = null;
      details = null;
      deleteArmed = false;
      await refresh();
    } catch (e) {
      showToast(`Delete failed: ${e}`);
    }
  }

  onMount(refresh);
  const selected = $derived(backups.find((b) => b.id === selectedId) ?? null);
  const previewCounts = $derived(preview ? countActions(preview.actions) : { create: 0, overwrite: 0 });
  const visibleEntries = $derived(details ? details.entries.slice(0, 14) : []);
</script>

<main class="shell">
  <header class="topbar">
    <div class="brand">
      <span class="logo" aria-hidden="true">▣</span>
      <div>
        <h1>ConfigVault</h1>
        <p>Versioned, verifiable backups of your configuration files</p>
      </div>
    </div>
    <div class="top-actions">
      <button class="btn btn-ghost" onclick={importBackup}>⬆ Import</button>
      <button class="btn" onclick={() => (newOpen = true)}>＋ New snapshot</button>
      <button class="icon-btn" title="Switch theme ({getPref()})" onclick={cycleTheme}>◐</button>
    </div>
  </header>

  <section class="layout">
    <aside class="sidebar card">
      <div class="sidebar-head">
        <span class="label">Snapshots</span>
        <span class="chip">{backups.length}</span>
      </div>
      {#if backups.length === 0}
        <div class="empty">
          <p>No snapshots yet.</p>
          <p class="dim">Back up your dotfiles and app configs — every file is checksummed and restores are preceded by an automatic safety backup.</p>
        </div>
      {:else}
        <ul class="list">
          {#each backups as b (b.id)}
            <li>
              <button class="row" class:active={b.id === selectedId} onclick={() => select(b.id)}>
                <span class="row-name">{b.name}</span>
                <span class="row-meta">{formatDate(b.created_at)}</span>
                <span class="row-meta dim">{b.file_count} files · {formatBytes(b.total_bytes)}</span>
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    </aside>

    <section class="detail card">
      {#if selected && details}
        <div class="detail-head">
          <div>
            <h2>{details.name}</h2>
            <p class="dim">Created {formatDate(details.created_at)} · {details.entries.length} files · {formatBytes(details.total_bytes)}</p>
          </div>
          <div class="detail-actions">
            <button class="btn" onclick={openRestorePreview}>↩ Restore…</button>
            <button class="btn btn-ghost" onclick={exportBackup}>⭳ Export</button>
            <button class="btn btn-danger" onclick={() => (deleteArmed ? confirmDelete() : armDelete())}>
              {deleteArmed ? 'Confirm delete' : 'Delete'}
            </button>
          </div>
        </div>

        <div class="meta-block">
          <span class="label">Sources</span>
          <ul class="srcs">{#each details.sources as s}<li title={s}>{shortPath(s)}</li>{/each}</ul>
          {#if details.excludes.length > 0}
            <span class="label">Excludes</span>
            <div class="chips">{#each details.excludes as e}<span class="chip">{e}</span>{/each}</div>
          {/if}
        </div>

        <div class="meta-block grow">
          <span class="label">Files <span class="dim">(sha256 verified on restore)</span></span>
          <ul class="files">
            {#each visibleEntries as e (e.target)}
              <li><code title={e.target}>{shortPath(e.target, 64)}</code><span class="dim">{formatBytes(e.size)}</span></li>
            {/each}
          </ul>
          {#if details.entries.length > visibleEntries.length}
            <p class="dim more">… and {details.entries.length - visibleEntries.length} more</p>
          {/if}
        </div>
      {:else}
        <div class="empty detail-empty">
          <p>Select a snapshot to inspect it, or create a new one.</p>
        </div>
      {/if}
    </section>
  </section>

  {#if newOpen}
    <div class="overlay" role="dialog" aria-modal="true">
      <div class="modal card">
        <h2>New snapshot</h2>
        <label class="field">
          <span class="label">Name</span>
          <input class="input" placeholder="e.g. dotfiles before upgrade" bind:value={snapName} />
        </label>
        <div class="field">
          <span class="label">Sources <span class="dim">(folders or files)</span></span>
          <div class="src-buttons">
            <button class="btn btn-ghost" onclick={addFolders}>＋ Folder…</button>
            <button class="btn btn-ghost" onclick={addFiles}>＋ Files…</button>
          </div>
          {#if snapSources.length > 0}
            <ul class="srcs">
              {#each snapSources as s}<li title={s}>{shortPath(s)} <button class="mini" onclick={() => (snapSources = snapSources.filter((x) => x !== s))}>✕</button></li>{/each}
            </ul>
          {:else}
            <p class="dim">Nothing selected yet.</p>
          {/if}
        </div>
        <label class="field">
          <span class="label">Exclude patterns <span class="dim">(one per line: name, *.ext, or /abs/prefix)</span></span>
          <textarea class="input" rows="5" bind:value={snapExcludes} spellcheck="false"></textarea>
        </label>
        <div class="modal-actions">
          <button class="btn btn-ghost" onclick={() => (newOpen = false)}>Cancel</button>
          <button class="btn" disabled={snapSources.length === 0 || creating} onclick={createSnapshot}>
            {creating ? 'Creating…' : 'Create snapshot'}
          </button>
        </div>
      </div>
    </div>
  {/if}

  {#if preview}
    <div class="overlay" role="dialog" aria-modal="true">
      <div class="modal card">
        <h2>Restore preview</h2>
        <p class="dim">
          {previewCounts.create} new file(s), {previewCounts.overwrite} overwrite(s).
          {#if preview.errors.length > 0} {preview.errors.length} invalid entr{preview.errors.length === 1 ? 'y' : 'ies'} will be skipped.{/if}
        </p>
        <p class="note">Existing files will first be copied into an automatic <strong>pre-restore backup</strong>. Nothing is deleted.</p>
        <ul class="files">
          {#each preview.actions.slice(0, 10) as a}
            <li><span class="chip kind-{a.kind}">{a.kind}</span><code title={a.target}>{shortPath(a.target, 56)}</code></li>
          {/each}
        </ul>
        {#if preview.actions.length > 10}<p class="dim more">… and {preview.actions.length - 10} more</p>{/if}
        {#if preview.errors.length > 0}
          <ul class="files err">{#each preview.errors.slice(0, 4) as e}<li><code>{e}</code></li>{/each}</ul>
        {/if}
        <div class="modal-actions">
          <button class="btn btn-ghost" onclick={() => { preview = null; previewFor = null; }}>Cancel</button>
          <button class="btn" disabled={restoring} onclick={confirmRestore}>{restoring ? 'Restoring…' : 'Restore now'}</button>
        </div>
      </div>
    </div>
  {/if}

  {#if toast}<div class="toast" role="status">{toast}</div>{/if}
</main>

<style>
  .shell { height: 100vh; display: flex; flex-direction: column; padding: 14px 18px 10px; gap: 12px; box-sizing: border-box; }
  .topbar { display: flex; align-items: center; justify-content: space-between; gap: 16px; }
  .brand { display: flex; align-items: center; gap: 12px; }
  .logo { font-size: 26px; color: var(--accent); }
  h1 { font-size: 19px; margin: 0; letter-spacing: 0.2px; }
  .brand p { margin: 2px 0 0; font-size: 12px; color: var(--text-dim, var(--muted, #8a8a93)); }
  .top-actions { display: flex; gap: 8px; align-items: center; }
  .layout { flex: 1; min-height: 0; display: grid; grid-template-columns: 330px 1fr; gap: 12px; }
  .sidebar { display: flex; flex-direction: column; min-height: 0; padding: 12px; }
  .sidebar-head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 8px; }
  .label { font-size: 11px; text-transform: uppercase; letter-spacing: 0.08em; color: var(--text-dim, var(--muted, #8a8a93)); }
  .list { list-style: none; margin: 0; padding: 0; overflow-y: auto; display: flex; flex-direction: column; gap: 6px; }
  .row { width: 100%; text-align: left; background: transparent; border: 1px solid transparent; border-radius: 10px; padding: 8px 10px; display: flex; flex-direction: column; gap: 2px; cursor: pointer; color: inherit; font: inherit; }
  .row:hover { background: var(--surface-hover, rgba(127, 127, 140, 0.12)); }
  .row.active { border-color: var(--accent); background: var(--surface-hover, rgba(127, 127, 140, 0.14)); }
  .row-name { font-weight: 600; font-size: 13px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .row-meta { font-size: 11px; }
  .dim { color: var(--text-dim, var(--muted, #8a8a93)); }
  .detail { display: flex; flex-direction: column; gap: 12px; min-height: 0; padding: 14px 16px; overflow-y: auto; }
  .detail-head { display: flex; justify-content: space-between; align-items: flex-start; gap: 12px; }
  .detail-head h2 { margin: 0 0 4px; font-size: 17px; }
  .detail-actions { display: flex; gap: 8px; flex-shrink: 0; }
  .meta-block { display: flex; flex-direction: column; gap: 6px; }
  .grow { flex: 1; min-height: 0; }
  .srcs, .files { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 4px; overflow-y: auto; }
  .srcs li { font-size: 12px; }
  .files li { display: flex; align-items: center; justify-content: space-between; gap: 10px; font-size: 12px; }
  .files code { background: var(--surface-hover, rgba(127, 127, 140, 0.12)); padding: 2px 6px; border-radius: 6px; font-size: 11px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .chips { display: flex; gap: 6px; flex-wrap: wrap; }
  .empty { color: var(--text-dim, var(--muted, #8a8a93)); font-size: 13px; padding: 14px; text-align: center; }
  .detail-empty { flex: 1; display: flex; align-items: center; justify-content: center; }
  .more { font-size: 11px; }
  .overlay { position: fixed; inset: 0; background: rgba(0, 0, 0, 0.45); display: flex; align-items: center; justify-content: center; z-index: 20; }
  .modal { width: min(520px, 92vw); max-height: 86vh; overflow-y: auto; padding: 18px 20px; display: flex; flex-direction: column; gap: 12px; }
  .modal h2 { margin: 0; font-size: 16px; }
  .field { display: flex; flex-direction: column; gap: 6px; }
  .src-buttons { display: flex; gap: 8px; }
  .modal-actions { display: flex; justify-content: flex-end; gap: 8px; }
  .mini { background: none; border: none; cursor: pointer; color: var(--text-dim, var(--muted, #8a8a93)); font-size: 11px; padding: 0 4px; }
  .note { font-size: 12px; background: var(--surface-hover, rgba(127, 127, 140, 0.12)); border-radius: 8px; padding: 8px 10px; margin: 0; }
  .kind-create { background: rgba(46, 204, 113, 0.18); color: #2ecc71; }
  .kind-overwrite { background: rgba(241, 196, 15, 0.18); color: #eab308; }
  .kind-skip { background: rgba(231, 76, 60, 0.18); color: #e74c3c; }
  .err code { color: #e74c3c; }
</style>
