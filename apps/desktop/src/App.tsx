import { Match, Switch, createEffect, on, onMount } from "solid-js";
import { Onboarding, Unlock } from "./auth";
import { Workspace } from "./layout";
import { refreshData, refreshStatus, status, wireEvents } from "./store";
import { ConfirmHost, Toasts } from "./ui";

export default function App() {
  onMount(async () => {
    wireEvents();
    await refreshStatus();
  });
  createEffect(on(() => status()?.unlocked, (unlocked) => unlocked && refreshData()));

  return (
    <>
      <Switch>
        <Match when={!status()}>
          <div class="auth"><span class="spinner" /></div>
        </Match>
        <Match when={!status()!.signed_in}>
          <Onboarding />
        </Match>
        <Match when={!status()!.unlocked}>
          <Unlock />
        </Match>
        <Match when={status()!.unlocked}>
          <Workspace />
        </Match>
      </Switch>
      <Toasts />
      <ConfirmHost />
    </>
  );
}
