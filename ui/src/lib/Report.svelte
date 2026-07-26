<script lang="ts">
  import { copyText, workReport } from "./api";
  import { highlight } from "./highlight";

  // The whole ticket workflow is copy-then-paste-into-chat, so Copy is the
  // highest-value control here and never more than one click away.
  const RANGES = [
    { value: "monday", label: "this week" },
    { value: "today", label: "today" },
    { value: "yesterday", label: "yesterday" },
    { value: "7d", label: "last 7 days" },
    { value: "30d", label: "last 30 days" },
    { value: "custom", label: "custom range" },
  ];

  const today = () => new Date().toISOString().slice(0, 10);

  let range = $state("monday");
  let from = $state(today());
  let to = $state(today());
  let project = $state("");
  let markdown = $state("");
  let loading = $state(false);
  let error = $state("");
  let copied = $state(false);

  const custom = $derived(range === "custom");
  const since = $derived(custom ? from : range);
  const until = $derived(custom ? to : null);
  const lines = $derived(markdown ? highlight(markdown) : []);

  async function load() {
    if (custom && (!from || !to)) {
      error = "Pick both a start and an end date.";
      return;
    }
    loading = true;
    error = "";
    try {
      markdown = await workReport(since, until, project.trim() || null);
    } catch (e) {
      error = String(e);
      markdown = "";
    } finally {
      loading = false;
    }
  }

  async function copy() {
    try {
      await copyText(markdown);
      copied = true;
      setTimeout(() => (copied = false), 1600);
    } catch (e) {
      error = String(e);
    }
  }

  $effect(() => {
    // Presets reload on change. A custom range waits for Refresh so a
    // half-typed date does not fire a report per keystroke.
    range;
    if (range !== "custom") void load();
  });
</script>

<div class="toolbar">
  <select bind:value={range} aria-label="Range">
    {#each RANGES as r (r.value)}
      <option value={r.value}>{r.label}</option>
    {/each}
  </select>

  {#if custom}
    <input type="date" bind:value={from} aria-label="From" max={to} />
    <span class="faint">→</span>
    <input type="date" bind:value={to} aria-label="To" min={from} />
  {/if}

  <input
    type="text"
    placeholder="project filter"
    bind:value={project}
    onkeydown={(e) => e.key === "Enter" && load()}
  />

  <button onclick={load} disabled={loading}>
    {loading ? "building" : "refresh"}
  </button>

  <span style="flex:1"></span>

  <button class="primary" onclick={copy} disabled={!markdown || loading}>
    {copied ? "copied" : "copy report"}
  </button>
</div>

{#if error}
  <div class="alert">{error}</div>
{/if}

{#if custom && !markdown && !error && !loading}
  <p class="blank">Pick two dates, then refresh. End dates are inclusive.</p>
{:else if loading && !markdown}
  <p class="blank">Building report…</p>
{:else if markdown}
  <div class="sheet">
    {#each lines as line}
      <div class={line.cls}>{#if line.marker}<span class="faint"
          >{line.marker}</span
        >{/if}{#each line.pieces as p}{#if p.code}<code>`{p.text}`</code
          >{:else}{p.text}{/if}{/each}</div>
    {/each}
  </div>
{:else if !error && !custom}
  <p class="blank">No sessions in range.</p>
{/if}
