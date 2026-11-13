import { Channel, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

// ---------------------------------------------------------------------------------------------
// Types (mirror the Rust structs)
// ---------------------------------------------------------------------------------------------

export type Uuid = string;
export type Role = "viewer" | "editor" | "admin" | "owner";

export interface Status {
  signed_in: boolean;
  unlocked: boolean;
  email: string | null;
  server_url: string | null;
  user_id: Uuid | null;
  device_id: Uuid | null;
  fingerprint: string | null;
  is_server_admin: boolean;
  /** Local-only mode: no sync server. */
  local: boolean;
  /** Saved server reached over plain http on the network. */
  insecure_server: boolean;
}

export interface MigrationOutcome {
  imported: number;
  skipped_duplicates: number;
  /** A new recovery key, shown once, when the previous one stopped working. */
  recovery_key: string | null;
  backup_path: string;
  status: Status;
}

export interface LocalSettings {
  last_server_url: string;
  last_email: string;
  mcp_enabled: boolean;
  mcp_port: number;
  mcp_token: string;
  mcp_allow_terminal_input: boolean;
  mcp_allow_writes: boolean;
}

export interface ServerInfo {
  name: string;
  version: string;
  api_version: number;
  server_id: string;
  signups: "open" | "invite-only" | "disabled";
  first_user: boolean;
}

export type Shell =
  | { kind: "pwsh" }
  | { kind: "windows_powershell" }
  | { kind: "cmd" }
  | { kind: "wsl"; distro: string }
  | { kind: "git_bash" }
  | { kind: "custom"; program: string };

export interface Host {
  label: string;
  address: string;
  port: number | null;
  protocol: "ssh" | "telnet";
  group_id: Uuid | null;
  identity_id: Uuid | null;
  username: string | null;
  password: string | null;
  ssh_key_id: Uuid | null;
  jump_host_id: Uuid | null;
  tags: string[];
  os: string;
  color: string;
  notes: string;
  startup_snippet_id: Uuid | null;
  keepalive_secs: number | null;
  agent_forwarding: boolean;
}
export interface Group { label: string; parent_id: Uuid | null; identity_id: Uuid | null; color: string }
export interface Identity { label: string; username: string; password: string | null; ssh_key_id: Uuid | null }
export interface SshKey { label: string; private_key: string; public_key: string; passphrase: string | null; certificate: string | null }
export interface KnownHost { host: string; key_type: string; fingerprint: string }
export interface Snippet { label: string; script: string; tags: string[] }
export interface PortForward {
  label: string;
  host_id: Uuid | null;
  kind: "local" | "remote" | "dynamic";
  bind_address: string;
  bind_port: number;
  dest_host: string;
  dest_port: number;
}
export interface TerminalProfile { label: string; shell: Shell; args: string[]; cwd: string; env: Record<string, string>; color: string }
export interface Workspace { label: string; tabs: { target: { kind: "profile"; profile_id: Uuid } | { kind: "host"; host_id: Uuid }; title: string }[] }
export interface Settings {
  theme: string;
  font_family: string;
  font_size: number;
  line_height: number;
  cursor_style: string;
  cursor_blink: boolean;
  scrollback: number;
  copy_on_select: boolean;
  default_profile_id: Uuid | null;
  lock_after_minutes: number;
}

export type Entity =
  | { type: "host"; data: Host }
  | { type: "group"; data: Group }
  | { type: "identity"; data: Identity }
  | { type: "ssh_key"; data: SshKey }
  | { type: "known_host"; data: KnownHost }
  | { type: "snippet"; data: Snippet }
  | { type: "port_forward"; data: PortForward }
  | { type: "terminal_profile"; data: TerminalProfile }
  | { type: "workspace"; data: Workspace }
  | { type: "settings"; data: Settings };
export type Kind = Entity["type"];

export interface Item<E extends Entity = Entity> {
  vault_id: Uuid;
  id: Uuid;
  version: number;
  pending: boolean;
  entity: E;
}

export interface VaultView {
  id: Uuid;
  name: string;
  description: string;
  color: string;
  personal: boolean;
  role: Role;
  rotating: boolean;
  member_count: number;
}

export interface MembersView {
  members: { user_id: Uuid; email: string; role: Role; fingerprint: string; is_me: boolean }[];
  pending: { user_id: Uuid; email: string; role: Role; fingerprint: string }[];
}

export interface InviteResponse { token: string; url: string; expires_at: number }
export interface DeviceInfo { id: Uuid; name: string; created_at: number; last_seen_at: number; current: boolean }
export interface SyncReport { pulled: number; pushed: number; conflicts: number; vaults: number }
export interface ConflictView {
  id: number;
  vault_id: Uuid;
  record_id: Uuid;
  at: number;
  label: string;
  conflicts: { field: string; local: unknown; remote: unknown }[];
}

export interface SessionInfo {
  id: Uuid;
  title: string;
  kind: { kind: "local"; profile_id: Uuid | null; shell: string; shell_kind?: Shell } | { kind: "ssh"; host_id: Uuid; address: string };
  alive: boolean;
  created_at: number;
  by_agent: boolean;
}

export interface DetectedShell { label: string; shell: Shell; program: string }
export interface ForwardStatus { rule_id: Uuid; label: string; description: string; running: boolean; error: string | null }

export interface McpInfo {
  enabled: boolean;
  running: boolean;
  url: string;
  token: string;
  allow_terminal_input: boolean;
  allow_writes: boolean;
  error: string | null;
  bridge_path: string | null;
}
export interface Activity { at: number; tool: string; summary: string; ok: boolean; reason: string | null; error: string | null }

export interface CmdError { code: string; message: string; details?: Record<string, unknown> }

export function errorMessage(e: unknown): string {
  if (e && typeof e === "object" && "message" in e) return String((e as CmdError).message);
  return String(e);
}
export function errorCode(e: unknown): string | undefined {
  return e && typeof e === "object" && "code" in e ? String((e as CmdError).code) : undefined;
}

// ---------------------------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------------------------

export const api = {
  status: () => invoke<Status>("status"),
  localSettings: () => invoke<LocalSettings>("local_settings"),
  serverInfo: (url: string) => invoke<ServerInfo>("server_info", { url }),
  signup: (url: string, email: string, password: string, invite?: string) =>
    invoke<string>("signup", { url, email, password, invite: invite || null }),
  login: (url: string, email: string, password: string) => invoke<Status>("login", { url, email, password }),
  recover: (url: string, email: string, recoveryKey: string, newPassword: string) =>
    invoke<string>("recover", { url, email, recoveryKey, newPassword }),
  unlock: (password: string) => invoke<Status>("unlock", { password }),
  createLocal: (name: string, password: string) => invoke<string>("create_local", { name, password }),
  recoverLocal: (recoveryKey: string, newPassword: string) => invoke<Status>("recover_local", { recoveryKey, newPassword }),
  connectNewAccount: (url: string, email: string, password: string, invite?: string) =>
    invoke<MigrationOutcome>("connect_new_account", { url, email, password, invite: invite || null }),
  connectExistingAccount: (url: string, email: string, password: string) =>
    invoke<MigrationOutcome>("connect_existing_account", { url, email, password }),
  disconnectToLocal: () => invoke<MigrationOutcome>("disconnect_to_local"),
  lock: () => invoke<Status>("lock"),
  logout: () => invoke<Status>("logout"),
  changePassword: (current: string, next: string) => invoke<void>("change_password", { current, new: next }),
  syncNow: () => invoke<SyncReport>("sync_now"),
  syncBackground: () => invoke<void>("sync_background"),
  devices: () => invoke<DeviceInfo[]>("devices"),
  revokeDevice: (deviceId: Uuid) => invoke<void>("revoke_device", { deviceId }),
  signupInvite: (email?: string) => invoke<InviteResponse>("signup_invite", { email: email || null }),

  items: (kind?: Kind) => invoke<Item[]>("items", { kind: kind ?? null }),
  saveItem: (vaultId: Uuid, id: Uuid | null, entity: Entity) => invoke<Uuid>("save_item", { vaultId, id, entity }),
  deleteItem: (vaultId: Uuid, id: Uuid) => invoke<void>("delete_item", { vaultId, id }),
  moveItem: (from: Uuid, to: Uuid, id: Uuid) => invoke<Uuid>("move_item", { from, to, id }),
  conflicts: () => invoke<ConflictView[]>("conflicts"),
  dismissConflict: (id: number) => invoke<void>("dismiss_conflict", { id }),

  vaults: () => invoke<VaultView[]>("vaults"),
  createVault: (name: string) => invoke<Uuid>("create_vault", { name }),
  updateVault: (vaultId: Uuid, name: string, description: string, color: string) =>
    invoke<void>("update_vault", { vaultId, name, description, color }),
  deleteVault: (vaultId: Uuid) => invoke<void>("delete_vault", { vaultId }),
  members: (vaultId: Uuid) => invoke<MembersView>("members", { vaultId }),
  invite: (vaultId: Uuid, role: Role, email?: string) => invoke<InviteResponse>("invite", { vaultId, role, email: email || null }),
  acceptInvite: (token: string) => invoke<Uuid | null>("accept_invite", { token }),
  approveMember: (vaultId: Uuid, userId: Uuid) => invoke<void>("approve_member", { vaultId, userId }),
  changeRole: (vaultId: Uuid, userId: Uuid, role: Role) => invoke<void>("change_role", { vaultId, userId, role }),
  removeMember: (vaultId: Uuid, userId: Uuid) => invoke<void>("remove_member", { vaultId, userId }),
  leaveVault: (vaultId: Uuid) => invoke<void>("leave_vault", { vaultId }),
  rotateVault: (vaultId: Uuid) => invoke<void>("rotate_vault", { vaultId }),
  fingerprint: () => invoke<string>("fingerprint"),

  detectShells: () => invoke<DetectedShell[]>("detect_shells"),
  openLocal: (opts: { profileId?: Uuid; shell?: Shell; confirmed?: boolean }, cols: number, rows: number) =>
    invoke<SessionInfo>("open_local", { profileId: opts.profileId ?? null, shell: opts.shell ?? null, cols, rows, confirmed: opts.confirmed ?? null }),
  openSsh: (hostId: Uuid, cols: number, rows: number, trustNew = false) =>
    invoke<SessionInfo>("open_ssh", { hostId, cols, rows, trustNew }),
  terminals: () => invoke<SessionInfo[]>("terminals"),
  attach: (id: Uuid, onData: (bytes: Uint8Array) => void) => {
    const channel = new Channel<ArrayBuffer>();
    channel.onmessage = (msg) => onData(new Uint8Array(msg));
    return invoke<void>("attach", { id, onData: channel });
  },
  write: (id: Uuid, data: string) => invoke<void>("write", { id, data }),
  resize: (id: Uuid, cols: number, rows: number) => invoke<void>("resize", { id, cols, rows }),
  closeTerminal: (id: Uuid) => invoke<void>("close_terminal", { id }),

  forwards: () => invoke<ForwardStatus[]>("forwards"),
  startForward: (ruleId: Uuid, trustNew = false) => invoke<ForwardStatus>("start_forward", { ruleId, trustNew }),
  stopForward: (ruleId: Uuid) => invoke<void>("stop_forward", { ruleId }),

  mcpStatus: () => invoke<McpInfo>("mcp_status"),
  mcpConfigure: (enabled: boolean, port: number, allowTerminalInput: boolean, allowWrites: boolean) =>
    invoke<McpInfo>("mcp_configure", { enabled, port, allowTerminalInput, allowWrites }),
  mcpRegenerateToken: () => invoke<McpInfo>("mcp_regenerate_token"),
  mcpActivity: () => invoke<Activity[]>("mcp_activity"),
};

export function on<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  return listen<T>(event, (e) => handler(e.payload));
}

// ---------------------------------------------------------------------------------------------
// Defaults for new items
// ---------------------------------------------------------------------------------------------

export const blank = {
  host: (): Host => ({
    label: "", address: "", port: null, protocol: "ssh", group_id: null, identity_id: null, username: "",
    password: null, ssh_key_id: null, jump_host_id: null, tags: [], os: "", color: "", notes: "",
    startup_snippet_id: null, keepalive_secs: null, agent_forwarding: false,
  }),
  group: (): Group => ({ label: "", parent_id: null, identity_id: null, color: "" }),
  identity: (): Identity => ({ label: "", username: "", password: null, ssh_key_id: null }),
  sshKey: (): SshKey => ({ label: "", private_key: "", public_key: "", passphrase: null, certificate: null }),
  snippet: (): Snippet => ({ label: "", script: "", tags: [] }),
  forward: (): PortForward => ({ label: "", host_id: null, kind: "local", bind_address: "127.0.0.1", bind_port: 8080, dest_host: "localhost", dest_port: 80 }),
  profile: (): TerminalProfile => ({ label: "", shell: { kind: "pwsh" }, args: [], cwd: "", env: {}, color: "" }),
};

export function shellLabel(shell: Shell): string {
  switch (shell.kind) {
    case "pwsh": return "PowerShell";
    case "windows_powershell": return "Windows PowerShell";
    case "cmd": return "Prompt de Comando";
    case "wsl": return `WSL (${shell.distro})`;
    case "git_bash": return "Git Bash";
    case "custom": return shell.program;
  }
}

export const roleLabel: Record<Role, string> = { viewer: "Leitor", editor: "Editor", admin: "Admin", owner: "Dono" };
