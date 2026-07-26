<script lang="ts">
  import { listen } from "@tauri-apps/api/event";
  import Report from "./lib/Report.svelte";
  import Sessions from "./lib/Sessions.svelte";
  import Settings from "./lib/Settings.svelte";
  import { listSessions, refreshNow, type SessionView } from "./lib/api";

  type Tab = "sessions" | "report" | "settings";

  let tab = $state<Tab>("sessions");
  let sessions = $state<SessionView[]>([]);
  let showAll = $state(false);
  let error = $state("");
  let busy = $state(false);
  let lastRefresh = $state(Date.now());
  let now = $state(Date.now());

  const needsAction = $derived(
    sessions.filter((s) => s.state === "NEEDS_ACTION").length,
  );

  // Seconds since the backend last handed us data. A monitoring tool that
  // cannot say how fresh it is has no business being trusted.
  const age = $derived(Math.max(0, Math.floor((now - lastRefresh) / 1000)));

  async function load(force = false) {
    busy = true;
    error = "";
    try {
      sessions = force ? await refreshNow() : await listSessions();
      lastRefresh = Date.now();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  $effect(() => {
    void load();

    // The backend watches Claude Code's runtime files and the spool, and
    // pushes when anything changes, so this does not poll aggressively.
    const unlisten = listen("ccmon://sessions-changed", () => void load());
    const poll = setInterval(() => void load(), 15_000);
    const clock = setInterval(() => (now = Date.now()), 1000);

    return () => {
      void unlisten.then((f) => f());
      clearInterval(poll);
      clearInterval(clock);
    };
  });
</script>

<nav class="bar">
  <button class="tab" class:on={tab === "sessions"} onclick={() => (tab = "sessions")}>
    Sessions{#if needsAction > 0}<span class="badge">{needsAction}</span>{/if}
  </button>
  <button class="tab" class:on={tab === "report"} onclick={() => (tab = "report")}>
    Report
  </button>
  <button class="tab" class:on={tab === "settings"} onclick={() => (tab = "settings")}>
    Settings
  </button>

  <span class="gap"></span>

  {#if tab === "sessions"}
    <button class="bare" onclick={() => (showAll = !showAll)}>
      {showAll ? "hide ended" : "show all"}
    </button>
    <span class="readout">
      {busy ? "syncing" : `${age}s`}<i class="caret"></i>
    </span>
    <button class="bare" onclick={() => load(true)} disabled={busy}>⟳</button>
  {/if}
</nav>

<main>
  {#if error && tab === "sessions"}
    <div class="alert">{error}</div>
  {/if}

  {#if tab === "sessions"}
    <Sessions {sessions} {showAll} />
  {:else if tab === "report"}
    <Report />
  {:else}
    <Settings />
  {/if}
</main>
