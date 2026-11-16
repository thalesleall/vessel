// Development-only: fakes the Tauri backend so the UI can be previewed in a plain browser
// (`npm run dev`, then open http://localhost:1420/?mock). Never bundled into the app.
import { mockIPC } from "@tauri-apps/api/mocks";

const personal = "0193a000-0000-7000-8000-000000000001";
const team = "0193a000-0000-7000-8000-000000000002";
const id = (n: number) => `0193b000-0000-7000-8000-${String(n).padStart(12, "0")}`;
let unlocked = new URLSearchParams(location.search).get("mock") !== "locked";

const host = (n: number, label: string, address: string, extra: Record<string, unknown> = {}) => ({
  vault_id: n > 4 ? team : personal, id: id(n), version: 1, pending: false,
  entity: { type: "host", data: {
    label, address, port: null, protocol: "ssh", group_id: null, identity_id: id(30), username: "deploy", password: null,
    ssh_key_id: null, jump_host_id: null, tags: [], os: "linux", color: "", notes: "", startup_snippet_id: null,
    keepalive_secs: null, ...extra,
  } },
});

const items = [
  { vault_id: personal, id: id(20), version: 1, pending: false, entity: { type: "group", data: { label: "Produção", parent_id: null, identity_id: null, color: "" } } },
  { vault_id: personal, id: id(21), version: 1, pending: false, entity: { type: "group", data: { label: "Homologação", parent_id: null, identity_id: null, color: "" } } },
  host(1, "api-prod-01", "10.0.1.11", { group_id: id(20), tags: ["prod"] }),
  host(2, "api-prod-02", "10.0.1.12", { group_id: id(20) }),
  host(3, "postgres-prod", "10.0.2.5", { group_id: id(20), port: 2222 }),
  host(4, "staging", "staging.exemplo.com", { group_id: id(21) }),
  host(5, "bastion", "203.0.113.10"),
  host(6, "grafana", "monitor.interno", { pending: true }),
  { vault_id: personal, id: id(30), version: 1, pending: false, entity: { type: "identity", data: { label: "deploy", username: "deploy", password: null, ssh_key_id: id(31) } } },
  { vault_id: personal, id: id(31), version: 1, pending: false, entity: { type: "ssh_key", data: { label: "id_ed25519", private_key: "-----BEGIN OPENSSH PRIVATE KEY-----", public_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHf0ZsN ana@pc", passphrase: "x", certificate: null } } },
  { vault_id: personal, id: id(40), version: 1, pending: false, entity: { type: "snippet", data: { label: "Status do nginx", script: "sudo systemctl status nginx", tags: [] } } },
  { vault_id: personal, id: id(41), version: 1, pending: false, entity: { type: "snippet", data: { label: "Uso de disco", script: "df -h | sort -k5 -r | head", tags: [] } } },
  { vault_id: personal, id: id(50), version: 1, pending: false, entity: { type: "terminal_profile", data: { label: "PowerShell", shell: { kind: "pwsh" }, args: [], cwd: "", env: {}, color: "" } } },
  { vault_id: personal, id: id(51), version: 1, pending: false, entity: { type: "terminal_profile", data: { label: "Ubuntu-24.04", shell: { kind: "wsl", distro: "Ubuntu-24.04" }, args: [], cwd: "", env: {}, color: "" } } },
  { vault_id: personal, id: id(52), version: 1, pending: false, entity: { type: "terminal_profile", data: { label: "Prompt de Comando", shell: { kind: "cmd" }, args: [], cwd: "", env: {}, color: "" } } },
  { vault_id: personal, id: id(60), version: 1, pending: false, entity: { type: "port_forward", data: { label: "Postgres prod", host_id: id(3), kind: "local", bind_address: "127.0.0.1", bind_port: 5433, dest_host: "localhost", dest_port: 5432 } } },
  { vault_id: personal, id: id(70), version: 1, pending: false, entity: { type: "settings", data: { theme: "midnight", font_family: "Cascadia Mono, Consolas, monospace", font_size: 14, line_height: 1.2, cursor_style: "bar", cursor_blink: true, scrollback: 10000, copy_on_select: false, default_profile_id: null, lock_after_minutes: 15 } } },
];

const sessions = [
  { id: id(90), title: "PowerShell", kind: { kind: "local", profile_id: id(50), shell: "PowerShell", shell_kind: { kind: "pwsh" } }, alive: true, created_at: 1, by_agent: false },
  { id: id(91), title: "api-prod-01", kind: { kind: "ssh", host_id: id(1), address: "deploy@10.0.1.11:22" }, alive: true, created_at: 2, by_agent: true },
];

// ?mock (server account), ?mock=local (local-only), ?mock=welcome (first run).
const mockMode = new URLSearchParams(location.search).get("mock") || "server";
let signedIn = mockMode !== "welcome";
let localMode = mockMode === "local" || mockMode === "welcome";

const status = () => ({
  signed_in: signedIn, unlocked: signedIn && unlocked,
  email: localMode ? "Ana" : "ana@exemplo.com", server_url: localMode ? "" : "https://vessel.exemplo.com",
  user_id: id(100), device_id: id(101), fingerprint: "04821 99130 57702 31468 80915", is_server_admin: !localMode, local: localMode,
});
const recoveryKey = "A1B2-C3D4-E5F6-0718-293A-4B5C-6D7E-8F90-1A2B-3C4D-5E6F-7081-92A3-B4C5-D6E7-F809";
const outcome = () => ({ imported: 14, skipped_duplicates: 3, recovery_key: localMode ? null : recoveryKey, backup_path: "C:\Users\ana\AppData\Roaming\dev.vessel.desktop\backups\vessel-1794540000-antes-de-conectar.db", status: status() });

const handlers: Record<string, (args: Record<string, unknown>) => unknown> = {
  status,
  local_settings: () => ({ last_server_url: "https://vessel.exemplo.com", last_email: "ana@exemplo.com", mcp_enabled: true, mcp_port: 47011, mcp_token: "vsl_9fK2mQ7xZpL0aB3cD5eF7gH9iJ1kL3mN5oP7qR9sT1u", mcp_allow_terminal_input: true, mcp_allow_writes: true }),
  unlock: () => { unlocked = true; return status(); },
  recover: () => { signedIn = true; unlocked = true; return recoveryKey; },
  create_local: () => { signedIn = true; localMode = true; unlocked = true; return recoveryKey; },
  recover_local: () => { unlocked = true; return status(); },
  server_info: () => ({ name: "Vessel", version: "0.1.0", api_version: 1, server_id: "vessel.exemplo.com", signups: "open", first_user: false }),
  connect_new_account: () => { localMode = false; return outcome(); },
  connect_existing_account: () => { localMode = false; const o = outcome(); o.recovery_key = null; return o; },
  disconnect_to_local: () => { localMode = true; return outcome(); },
  lock: () => { unlocked = false; return status(); },
  items: (a) => (a.kind ? items.filter((i) => i.entity.type === a.kind) : items),
  vaults: () => [
    { id: personal, name: "Pessoal", description: "", color: "", personal: true, role: "owner", rotating: false, member_count: 1 },
    { id: team, name: "Infra", description: "", color: "", personal: false, role: "owner", rotating: false, member_count: 3 },
  ],
  terminals: () => sessions,
  members: () => ({
    members: [
      { user_id: id(100), email: "ana@exemplo.com", role: "owner", fingerprint: "04821 99130 57702 31468 80915", is_me: true },
      { user_id: id(102), email: "ana@exemplo.com", role: "admin", fingerprint: "11873 40291 88812 03917 66104", is_me: false },
      { user_id: id(103), email: "bruno@exemplo.com", role: "editor", fingerprint: "73019 22847 51290 84402 19385", is_me: false },
    ],
    pending: [{ user_id: id(104), email: "carla@exemplo.com", role: "editor", fingerprint: "38120 77401 90235 11847 60293" }],
  }),
  detect_shells: () => [
    { label: "PowerShell", shell: { kind: "pwsh" }, program: "C:\\Program Files\\PowerShell\\7\\pwsh.exe" },
    { label: "Prompt de Comando", shell: { kind: "cmd" }, program: "C:\\Windows\\System32\\cmd.exe" },
    { label: "Ubuntu-24.04", shell: { kind: "wsl", distro: "Ubuntu-24.04" }, program: "C:\\Windows\\System32\\wsl.exe" },
    { label: "Debian", shell: { kind: "wsl", distro: "Debian" }, program: "C:\\Windows\\System32\\wsl.exe" },
    { label: "Windows PowerShell", shell: { kind: "windows_powershell" }, program: "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe" },
    { label: "Git Bash", shell: { kind: "git_bash" }, program: "C:\\Program Files\\Git\\bin\\bash.exe" },
  ],
  devices: () => [
    { id: id(101), name: "DESKTOP-ANA", created_at: 0, last_seen_at: Math.floor(Date.now() / 1000), current: true },
    { id: id(105), name: "NOTEBOOK", created_at: 0, last_seen_at: Math.floor(Date.now() / 1000) - 7200, current: false },
  ],
  conflicts: () => [],
  forwards: () => [{ rule_id: id(60), label: "Postgres prod", description: "", running: true, error: null }],
  mcp_status: () => ({ enabled: true, running: true, url: "http://127.0.0.1:47011/mcp", token: "vsl_9fK2mQ7xZpL0aB3cD5eF7gH9iJ1kL3mN5oP7qR9sT1u", allow_terminal_input: true, allow_writes: true, error: null, bridge_path: "C:\\Program Files\\Vessel\\vessel-mcp.exe" }),
  mcp_activity: () => [
    { at: Math.floor(Date.now() / 1000) - 40, tool: "terminal_run", summary: "session_id=… command=df -h", ok: true, reason: null, error: null },
    { at: Math.floor(Date.now() / 1000) - 95, tool: "connect_ssh", summary: "host_id=api-prod-01", ok: true, reason: null, error: null },
    { at: Math.floor(Date.now() / 1000) - 300, tool: "create_item", summary: "kind=host", ok: true, reason: null, error: null },
    { at: Math.floor(Date.now() / 1000) - 320, tool: "create_item", summary: "kind=identity", ok: false, reason: "Só você pode alterar esse tipo de item", error: "agents cannot create identity items" },
  ],
  attach: (a) => {
    const channel = a.onData as { id: number };
    const internals = (window as unknown as { __TAURI_INTERNALS__: { runCallback: (id: number, data: unknown) => void } }).__TAURI_INTERNALS__;
    const text = a.id === id(91)
      ? "Welcome to Ubuntu 24.04.1 LTS (GNU/Linux 6.8.0-45-generic x86_64)\r\n\r\n\x1b[32mdeploy@api-prod-01\x1b[0m:\x1b[34m~\x1b[0m$ df -h\r\nFilesystem      Size  Used Avail Use% Mounted on\r\n/dev/sda1        80G   31G   46G  41% /\r\n\x1b[32mdeploy@api-prod-01\x1b[0m:\x1b[34m~\x1b[0m$ "
      : "PowerShell 7.5.2\r\n\x1b[32mPS C:\\Users\\ana>\x1b[0m ";
    setTimeout(() => internals.runCallback(channel.id, { index: 0, message: new TextEncoder().encode(text).buffer }), 50);
    return null;
  },
};

mockIPC((cmd, args) => {
  if (cmd.startsWith("plugin:event|")) return 1;
  const h = handlers[cmd];
  return h ? h((args ?? {}) as Record<string, unknown>) : null;
});
