import { For, Show, createResource, createSignal } from "solid-js";
import { api, blank, shellLabel, type Item, type Shell, type TerminalProfile } from "../api";
import { itemsOf } from "../store";
import { openProfile, openShell } from "../terminal";
import { Button, Empty, Field, Icon, Modal, ShellIcon } from "../ui";
import { EditorFooter, VaultSelect, defaultVault, removeItem, saveEntity } from "./common";
import { invalid } from "../validate";

type ProfileItem = Item<{ type: "terminal_profile"; data: TerminalProfile }>;

export function ProfilesView() {
  const profiles = itemsOf("terminal_profile");
  const [shells] = createResource(() => api.detectShells());
  const [editing, setEditing] = createSignal<ProfileItem | "new" | null>(null);

  return (
    <>
      <div class="page-head">
        <h1>Terminais locais</h1>
        <div class="spacer" />
        <Button variant="primary" icon="plus" onClick={() => setEditing("new")}>Novo perfil</Button>
      </div>
      <p class="subtitle">Perfis sincronizam entre seus computadores; o terminal sempre roda na máquina em que você está.</p>

      <Show when={profiles().length > 0} fallback={<Empty icon="terminal" title="Nenhum perfil"><span>Crie um perfil a partir dos shells detectados abaixo.</span></Empty>}>
        <div class="grid">
          <For each={profiles()}>
            {(p) => (
              <div class="card">
                <button class="card-hit" onClick={() => openProfile(p.id)} aria-label={`Abrir ${p.entity.data.label}`} title="Abrir terminal" />
                <ShellIcon shell={p.entity.data.shell} color={p.entity.data.color} size={38} />
                <div class="meta">
                  <div class="name">{p.entity.data.label}</div>
                  <div class="sub">{shellLabel(p.entity.data.shell)}{p.entity.data.cwd ? `, ${p.entity.data.cwd}` : ""}</div>
                </div>
                <div class="actions">
                  <button class="icon-btn" aria-label="Editar" title="Editar" onClick={() => setEditing(p)}><Icon name="edit" size={15} /></button>
                </div>
              </div>
            )}
          </For>
        </div>
      </Show>

      <div class="section-title">Detectados neste computador</div>
      <div class="list">
        <Show when={!shells.loading} fallback={<div class="row"><span class="spinner" /></div>}>
          <For each={shells() ?? []}>
            {(s) => (
              <div class="row">
                <ShellIcon shell={s.shell} size={26} />
                <div class="meta"><div class="name">{s.label}</div><div class="sub mono">{s.program}</div></div>
                <Button small icon="play" onClick={() => openShell(s.shell)}>Abrir</Button>
              </div>
            )}
          </For>
        </Show>
      </div>

      <Show when={editing()} keyed>
        {(e) => <ProfileEditor item={e === "new" ? null : (e as ProfileItem)} shells={(shells() ?? []).map((s) => s.shell)} onClose={() => setEditing(null)} />}
      </Show>
    </>
  );
}

function shellKey(s: Shell): string {
  return s.kind === "wsl" ? `wsl:${s.distro}` : s.kind === "custom" ? "custom" : s.kind;
}

function ProfileEditor(props: { item: ProfileItem | null; shells: Shell[]; onClose: () => void }) {
  const [d, setD] = createSignal<TerminalProfile>(props.item ? { ...props.item.entity.data } : blank.profile());
  const [vault, setVault] = createSignal(props.item?.vault_id ?? defaultVault());
  const [args, setArgs] = createSignal(d().args.join(" "));
  const [env, setEnv] = createSignal(Object.entries(d().env).map(([k, v]) => `${k}=${v}`).join("\n"));
  const options = () => {
    const base: Shell[] = [{ kind: "pwsh" }, { kind: "windows_powershell" }, { kind: "cmd" }, { kind: "git_bash" }];
    const all = [...base, ...props.shells.filter((s) => s.kind === "wsl")];
    if (d().shell.kind === "wsl" && !all.some((s) => shellKey(s) === shellKey(d().shell))) all.push(d().shell);
    return all;
  };
  const choose = (key: string) => {
    if (key === "custom") setD({ ...d(), shell: { kind: "custom", program: (() => { const s = d().shell; return s.kind === "custom" ? s.program : ""; })() } });
    else setD({ ...d(), shell: options().find((s) => shellKey(s) === key) ?? { kind: "pwsh" } });
  };
  const save = async () => {
    const shell = d().shell;
    if (shell.kind === "custom" && !shell.program.trim()) return invalid("Programa", "Programa: não pode ficar em branco. Informe o caminho do executável.");
    const envMap: Record<string, string> = {};
    for (const [n, raw] of env().split("\n").entries()) {
      const line = raw.replace(/\r$/, "");
      if (!line.trim()) continue;
      const i = line.indexOf("=");
      const name = i > 0 ? line.slice(0, i).trim() : "";
      if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) {
        return invalid("Variáveis de ambiente", `Variável inválida na linha ${n + 1} ("${line.trim().slice(0, 40)}"): use NOME=valor, com o nome feito de letras, números e _.`);
      }
      envMap[name] = line.slice(i + 1);
    }
    const profile: TerminalProfile = {
      ...d(), label: d().label.trim() || shellLabel(d().shell), args: args().split(" ").filter(Boolean), env: envMap,
    };
    if (await saveEntity(props.item, vault(), { type: "terminal_profile", data: profile })) props.onClose();
  };
  return (
    <Modal title={props.item ? "Editar perfil" : "Novo perfil"} onClose={props.onClose}
      footer={<EditorFooter onCancel={props.onClose} onSave={save}
        onDelete={props.item ? async () => { await removeItem(props.item!, "o perfil"); props.onClose(); } : undefined} />}>
      <div class="form">
        <Field label="Nome"><input value={d().label} onInput={(e) => setD({ ...d(), label: e.currentTarget.value })} placeholder={shellLabel(d().shell)} autofocus /></Field>
        <Field label="Shell">
          <select value={shellKey(d().shell)} onChange={(e) => choose(e.currentTarget.value)}>
            <For each={options()}>{(s) => <option value={shellKey(s)}>{shellLabel(s)}</option>}</For>
            <option value="custom">Programa personalizado…</option>
          </select>
        </Field>
        <Show when={d().shell.kind === "custom"}>
          <Field label="Programa"><input class="mono" value={d().shell.kind === "custom" ? (d().shell as { program: string }).program : ""}
            onInput={(e) => setD({ ...d(), shell: { kind: "custom", program: e.currentTarget.value } })} placeholder="C:\\caminho\\programa.exe" /></Field>
        </Show>
        <Field label="Argumentos"><input class="mono" value={args()} onInput={(e) => setArgs(e.currentTarget.value)} /></Field>
        <Field label="Pasta inicial" hint="Vazio = sua pasta de usuário."><input class="mono" value={d().cwd} onInput={(e) => setD({ ...d(), cwd: e.currentTarget.value })} /></Field>
        <Field label="Variáveis de ambiente" hint="Uma por linha: NOME=valor"><textarea value={env()} onInput={(e) => setEnv(e.currentTarget.value)} /></Field>
        <Field label="Cofre"><VaultSelect value={vault()} onChange={setVault} /></Field>
      </div>
    </Modal>
  );
}
