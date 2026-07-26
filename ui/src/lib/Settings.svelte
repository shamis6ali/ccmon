<script lang="ts">
  import { dataDir, getSettings, setSettings, type AppSettings } from "./api";

  let settings = $state<AppSettings | null>(null);
  let dir = $state("");
  let error = $state("");
  let saved = $state(false);

  $effect(() => {
    void (async () => {
      try {
        settings = await getSettings();
        dir = await dataDir();
      } catch (e) {
        error = String(e);
      }
    })();
  });

  async function save() {
    if (!settings) return;
    error = "";
    try {
      await setSettings($state.snapshot(settings));
      saved = true;
      setTimeout(() => (saved = false), 1600);
    } catch (e) {
      error = String(e);
    }
  }
</script>

{#if error}
  <div class="alert">{error}</div>
{/if}

{#if settings}
  <div class="opt">
    <div>
      <div class="k">Notify on needs-action</div>
      <p class="why">
        Fires when a session starts waiting on you. Never for needs-review or
        idle — across this many sessions that would be constant noise, and the
        tray badge already carries it.
      </p>
    </div>
    <div class="ctl">
      <input
        type="checkbox"
        bind:checked={settings.notifications_enabled}
        onchange={save}
      />
    </div>
  </div>

  <div class="opt">
    <div>
      <div class="k">Start at login</div>
      <p class="why">Launches minimised to the tray.</p>
    </div>
    <div class="ctl">
      <input type="checkbox" bind:checked={settings.autostart} onchange={save} />
    </div>
  </div>

  <div class="opt">
    <div>
      <div class="k">Stale after</div>
      <p class="why">
        Days without activity before a session is flagged. Staleness is a flag
        rather than a state, because stale needs-review and stale dead are
        different problems.
      </p>
    </div>
    <div class="ctl">
      <input
        type="number"
        min="1"
        max="365"
        style="width:64px"
        bind:value={settings.stale_after_days}
        onchange={save}
      />
    </div>
  </div>

  <p class="faint" style="margin-top:16px; font-size:11px">
    {saved ? "saved" : "changes save immediately"}
  </p>

  <div style="margin-top:26px">
    <div class="k" style="font-size:10px; color:var(--faint)">Data directory</div>
    <p class="path" style="margin:4px 0 0">{dir}</p>
  </div>

  <p class="faint" style="margin-top:20px; max-width:56ch; font-size:11px">
    ccmon reads Claude Code's files and never writes to them. No network calls,
    no LLM, no telemetry.
  </p>
{:else if !error}
  <p class="blank">Loading…</p>
{/if}
