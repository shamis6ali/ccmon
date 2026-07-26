<script lang="ts">
  import {
    ACTION_LABEL,
    STATE_ORDER,
    basename,
    copySessionId,
    inStaleGroup,
    openPath,
    primaryProject,
    relativeTime,
    resumeSession,
    title,
    type SessionState,
    type SessionView,
  } from "./api";

  let { sessions, showAll = false }: { sessions: SessionView[]; showAll?: boolean } =
    $props();

  let error = $state("");

  // Ended and dead sessions are not triage; they are one toggle away.
  const visible = $derived(
    showAll
      ? sessions
      : sessions.filter((s) => s.state !== "ENDED" && s.state !== "DEAD"),
  );

  const groups = $derived(
    STATE_ORDER.map((state) => ({
      state,
      rows: visible.filter((s) => s.state === state),
    })).filter((g) => g.rows.length > 0),
  );

  const colour = (s: SessionState) => `var(--sig-${s.toLowerCase().replace("_", "-")})`;
  const label = (s: SessionState) => s.replace("_", " ");
  // Zero-padded, because a column of aligned digits is the point of a readout.
  const pad = (n: number) => String(n).padStart(2, "0");

  async function guard(fn: () => Promise<unknown>) {
    error = "";
    try {
      await fn();
    } catch (e) {
      error = String(e);
    }
  }
</script>

{#if error}
  <div class="alert">{error}</div>
{/if}

{#if groups.length === 0}
  <p class="blank">
    No sessions{#if !showAll} — ended and dead ones are hidden{/if}
  </p>
{:else}
  {#each groups as group (group.state)}
    <section class="group" style="--st: {colour(group.state)}">
      <div class="ghead">
        <span class="name">{label(group.state)}</span>
        <span class="rule"></span>
        <span class="n">{pad(group.rows.length)}</span>
      </div>

      {#each group.rows as s, i (s.session_id)}
        {@const project = primaryProject(s)}
        {@const alive = s.liveness === "alive"}
        <article class="row" style="animation-delay: {Math.min(i, 12) * 22}ms">
          <div class="line1">
            {#if s.state === "WORKING"}<i class="live"></i>{/if}
            <span class="title" title={title(s)}>{title(s)}</span>
            <span class="age">{relativeTime(s.last_event_at)}</span>
          </div>

          <div class="line2">
            <span>{basename(project)}</span>

            {#if s.git_branch && s.git_branch !== "HEAD" && s.worktree_dirty !== null}
              <span class="dot">·</span><span>{s.git_branch}</span>
            {/if}
            {#if s.term_program}
              <span class="dot">·</span><span>{s.term_program}</span>
            {/if}

            {#if s.action_kind}
              <span class="dot">·</span>
              <span class="flag">{ACTION_LABEL[s.action_kind]}</span>
            {/if}
            {#if inStaleGroup(s)}
              <span class="dot">·</span><span class="flag warn">stale</span>
            {/if}
            {#if s.worktree_dirty}
              <span class="dot">·</span><span class="flag warn">dirty</span>
            {/if}
            {#if s.open_todos > 0}
              <span class="dot">·</span>
              <span class="flag mute">{s.open_todos} todo</span>
            {/if}
            {#if s.runtime_kind === "bg"}
              <span class="dot">·</span><span class="flag mute">bg</span>
            {/if}
          </div>

          {#if s.first_prompt}
            <p class="ask">{s.first_prompt}</p>
          {/if}

          <div class="acts">
            <!-- Resume stays disabled while the process is alive: two Claude
                 Code processes on one session file corrupt the transcript, and
                 that is exactly the case where the window can be found by its
                 title instead. -->
            <button
              disabled={alive}
              title={alive
                ? `Still running${s.term_program ? ` in ${s.term_program}` : ""} — look for the window titled “${title(s)}”. Resuming a live session would corrupt its transcript.`
                : "Resume in a new terminal"}
              onclick={() => guard(() => resumeSession(s.session_id))}
            >
              resume
            </button>
            <button class="bare" onclick={() => guard(() => openPath(project))}>
              folder
            </button>
            <button class="bare" onclick={() => guard(() => copySessionId(s.session_id))}>
              copy id
            </button>
          </div>
        </article>
      {/each}
    </section>
  {/each}
{/if}
