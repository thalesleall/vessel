import { For, Show, createResource, createSignal, onCleanup } from "solid-js";
import { api, blank, type Item, type PortForward } from "../api";
import { byId, itemsOf, toast } from "../store";
import { withHostKeyCheck } from "../terminal";
import { Badge, Button, Empty, Field, Icon, Modal, confirmAction } from "../ui";
import { EditorFooter, ItemSelect, VaultSelect, defaultVault, labelOf, removeItem, saveEntity } from "./common";

type ForwardItem = Item<{ type: "port_forward"; data: PortForward }>;

function describe(r: PortForward) {
  const bind = r.bind_address || "127.0.0.1";
  if (r.kind === "dynamic") return `SOCKS5 em ${bind}:${r.bind_port}`;
  if (r.kind === "remote") return `remoto ${bind}:${r.bind_port} → ${r.dest_host}:${r.dest_port}`;
  return `${bind}:${r.bind_port} → ${r.dest_host}:${r.dest_port}`;
}

/** A local or SOCKS listener reachable from other machines on the network. */
function exposed(r: PortForward) {
  const bind = (r.bind_address || "127.0.0.1").trim();
  return r.kind !== "remote" && !["127.0.0.1", "localhost", "::1", "[::1]"].includes(bind);
}

export function ForwardsView() {
  const rules = itemsOf("port_forward");
  const [status, { refetch }] = createResource(() => api.forwards());
  const timer = setInterval(refetch, 3000);
  onCleanup(() => clearInterval(timer));
  const [editing, setEditing] = createSignal<ForwardItem | "new" | null>(null);
  const running = (id: string) => status()?.find((s) => s.rule_id === id);

  const start = async (r: ForwardItem) => {
    const hostId = r.entity.data.host_id;
    if (!hostId) return toast("Escolha um host para a regra.", "error");
    if (exposed(r.entity.data) && !(await confirmAction({
      title: "Abrir a porta para a rede?",
      message: `Esta regra escuta em ${r.entity.data.bind_address}:${r.entity.data.bind_port}, não só neste computador. Outros computadores da rede vão poder usar o túnel${r.entity.data.kind === "dynamic" ? " (um proxy SOCKS sem senha)" : ""}.`,
      confirm: "Iniciar mesmo assim",
    }))) return;
    await withHostKeyCheck(hostId, async (trust) => {
      await api.startForward(r.id, trust);
      toast(`Redirecionamento ativo: ${describe(r.entity.data)}`, "success");
    });
    refetch();
  };

  return (
    <>
      <div class="page-head">
        <h1>Redirecionamento de portas</h1>
        <div class="spacer" />
        <Button variant="primary" icon="plus" onClick={() => setEditing("new")}>Nova regra</Button>
      </div>
      <Show when={rules().length} fallback={<Empty icon="route" title="Nenhuma regra"><span>Túneis locais (-L), remotos (-R) e proxy SOCKS (-D) pelos seus hosts.</span></Empty>}>
        <div class="list">
          <For each={rules()}>
            {(r) => {
              const st = () => running(r.id);
              const host = () => byId(r.entity.data.host_id);
              return (
                <div class="row">
                  <Icon name="route" />
                  <div class="meta">
                    <div class="name">{r.entity.data.label || describe(r.entity.data)}</div>
                    <div class="sub">{describe(r.entity.data)}, via {host() ? labelOf(host()!) : "?"}</div>
                    <Show when={exposed(r.entity.data)}><div class="sub" style={{ color: "var(--warn)" }}>Aberto para a rede</div></Show>
                  </div>
                  <Show when={st()?.running}><Badge tone="green">ativo</Badge></Show>
                  <Show when={st()?.error}><Badge tone="red">{st()!.error}</Badge></Show>
                  <Show when={st()?.running} fallback={<Button small icon="play" onClick={() => start(r)}>Iniciar</Button>}>
                    <Button small icon="stop" onClick={async () => { await api.stopForward(r.id); refetch(); }}>Parar</Button>
                  </Show>
                  <Button small icon="edit" onClick={() => setEditing(r)}>Editar</Button>
                </div>
              );
            }}
          </For>
        </div>
      </Show>
      <Show when={editing()} keyed>{(e) => <ForwardEditor item={e === "new" ? null : (e as ForwardItem)} onClose={() => setEditing(null)} />}</Show>
    </>
  );
}

function ForwardEditor(props: { item: ForwardItem | null; onClose: () => void }) {
  const [d, setD] = createSignal<PortForward>(props.item ? { ...props.item.entity.data } : blank.forward());
  const [vault, setVault] = createSignal(props.item?.vault_id ?? defaultVault());
  const set = <K extends keyof PortForward>(k: K, v: PortForward[K]) => setD({ ...d(), [k]: v });
  const save = async () => {
    if (!d().host_id) return toast("Escolha o host.", "error");
    if (await saveEntity(props.item, vault(), { type: "port_forward", data: d() })) props.onClose();
  };
  return (
    <Modal title={props.item ? "Editar regra" : "Nova regra"} onClose={props.onClose}
      footer={<EditorFooter onCancel={props.onClose} onSave={save}
        onDelete={props.item ? async () => { await removeItem(props.item!, "a regra"); props.onClose(); } : undefined} />}>
      <div class="form">
        <Field label="Tipo">
          <div class="segmented">
            <button classList={{ on: d().kind === "local" }} onClick={() => set("kind", "local")}>Local (-L)</button>
            <button classList={{ on: d().kind === "remote" }} onClick={() => set("kind", "remote")}>Remoto (-R)</button>
            <button classList={{ on: d().kind === "dynamic" }} onClick={() => set("kind", "dynamic")}>SOCKS (-D)</button>
          </div>
        </Field>
        <Field label="Nome"><input value={d().label} onInput={(e) => set("label", e.currentTarget.value)} placeholder="ex.: Postgres prod" /></Field>
        <Field label="Host SSH"><ItemSelect kind="host" value={d().host_id} onChange={(v) => set("host_id", v)} none="Escolha…" /></Field>
        <div class="form-row">
          <Field label={d().kind === "remote" ? "Endereço no servidor" : "Endereço local"}><input value={d().bind_address} onInput={(e) => set("bind_address", e.currentTarget.value)} /></Field>
          <Field label="Porta"><input type="number" value={d().bind_port} onInput={(e) => set("bind_port", Number(e.currentTarget.value))} /></Field>
        </div>
        <Show when={d().kind !== "dynamic"}>
          <div class="form-row">
            <Field label="Destino"><input value={d().dest_host} onInput={(e) => set("dest_host", e.currentTarget.value)} /></Field>
            <Field label="Porta de destino"><input type="number" value={d().dest_port} onInput={(e) => set("dest_port", Number(e.currentTarget.value))} /></Field>
          </div>
        </Show>
        <Field label="Cofre"><VaultSelect value={vault()} onChange={setVault} /></Field>
      </div>
    </Modal>
  );
}
