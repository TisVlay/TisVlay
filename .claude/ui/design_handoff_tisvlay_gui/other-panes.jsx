// Profiles + About + Session panes for tisvlay GUI.

const SYSTEM_PROFILE_FIELDS = [
  { key: "on_startup",    label: "On system startup",   default: "power_on",  helper: "WoL → connect → screen on → input switch." },
  { key: "on_shutdown",   label: "On shutdown",         default: "power_off", helper: "Fired during poweroff/halt." },
  { key: "on_reboot",     label: "On reboot",           default: "none",      helper: "Reboot/kexec; TV stays on for BIOS/TTY." },
  { key: "on_no_session", label: "When all sessions end", default: "none",    helper: "Login screen / after logout." },
  { key: "on_sleep",      label: "On suspend",          default: "power_off", helper: "Network is unavailable during sleep." },
  { key: "on_wake",       label: "On resume",           default: "power_on",  helper: "Backend handles WoL retry for lost packets." },
];

const USER_PROFILE_FIELDS = [
  { key: "on_session_lock",     label: "On session lock",   default: "screen_off", helper: "Screen blanks; instant resume on unlock." },
  { key: "on_session_unlock",   label: "On session unlock", default: "screen_on",  helper: "Skipped if greeter-wake already unblanked." },
  { key: "on_greeter_wake",     label: "On greeter wake",   default: "screen_on",  helper: "Lock-screen interaction (Wayland only)." },
  { key: "idle_timeout_seconds",label: "Idle timeout",      default: 0,            helper: "Seconds of inactivity before idle action. 0 disables.", numeric: true },
  { key: "on_idle_timeout",     label: "On idle timeout",   default: "none",       helper: "Action when idle threshold is reached." },
  { key: "on_idle_resume",      label: "On idle resume",    default: "screen_on",  helper: "Action when activity resumes." },
];

const ACTION_OPTIONS = [
  { value: "power_on",   label: "Power on" },
  { value: "power_off",  label: "Power off" },
  { value: "screen_on",  label: "Screen on" },
  { value: "screen_off", label: "Screen off" },
  { value: "none",       label: "None" },
];

const AUTOMATE_WHEN_OPTIONS = [
  { value: "always",      label: "Always" },
  { value: "on_primary",  label: "Only when on primary input" },
  { value: "off_primary", label: "Only when not on primary input" },
];

const SAMPLE_SYSTEM_PROFILES = {
  default: { _is_default: true, on_startup: "power_on", on_shutdown: "power_off", on_reboot: "none", on_no_session: "none", on_sleep: "power_off", on_wake: "power_on", automate_when: "always" },
  shared:  { automate_when: "on_primary" },
  guest:   { on_startup: "none", on_shutdown: "screen_off" },
};
const SAMPLE_USER_PROFILES = {
  default:    { _is_default: true, on_session_lock: "screen_off", on_session_unlock: "screen_on", on_greeter_wake: "screen_on", idle_timeout_seconds: 0, on_idle_timeout: "none", on_idle_resume: "screen_on" },
  gaming:     { on_session_lock: "none", idle_timeout_seconds: 0, on_idle_resume: "none" },
  "movie-night": { idle_timeout_seconds: 0, on_idle_timeout: "none" },
};

// =========================================================================
// Profiles pane
// =========================================================================
function ProfilesPane({ theme }) {
  const t = APP_T[theme];
  const [tab, setTab] = React.useState("system");
  const [selectedSystem, setSelectedSystem] = React.useState("default");
  const [selectedUser, setSelectedUser] = React.useState("default");

  const profiles = tab === "system" ? SAMPLE_SYSTEM_PROFILES : SAMPLE_USER_PROFILES;
  const selected = tab === "system" ? selectedSystem : selectedUser;
  const setSelected = tab === "system" ? setSelectedSystem : setSelectedUser;

  return (
    <div style={{ flex: 1, display: "flex", flexDirection: "column", minWidth: 0, background: t.surface }}>
      <PaneHeader theme={theme}
        title="Profiles"
        subtitle="System profiles drive daemon lifecycle. User profiles drive desktop session events. Each is per-TV.">
      </PaneHeader>

      {/* Tabs */}
      <div style={{ padding: "12px 24px 0", borderBottom: `1px solid ${t.border}`, display: "flex", gap: 4, background: t.surface, flexShrink: 0 }}>
        <ProfileTab theme={theme} active={tab === "system"} onClick={() => setTab("system")}
          label="System profiles" sub="boot, shutdown, sleep" />
        <ProfileTab theme={theme} active={tab === "user"} onClick={() => setTab("user")}
          label="User profiles" sub="lock, unlock, idle" />
      </div>

      <div style={{ flex: 1, display: "flex", minHeight: 0, background: t.surfaceAlt }}>
        {/* Master list */}
        <div style={{ width: 240, flexShrink: 0, borderRight: `1px solid ${t.border}`, background: t.surfaceAlt, display: "flex", flexDirection: "column" }}>
          <div style={{ padding: "10px 12px", display: "flex", alignItems: "center", justifyContent: "space-between", borderBottom: `1px solid ${t.border}`, background: t.surface }}>
            <span style={{ fontSize: 11, fontWeight: 600, color: t.textDim, textTransform: "uppercase", letterSpacing: 0.6 }}>
              {Object.keys(profiles).length} profiles
            </span>
            <IconBtn theme={theme} variant="accent" title="Create profile">
              <SidebarIcon name="plus" size={14} />
            </IconBtn>
          </div>
          <div style={{ flex: 1, overflow: "auto" }}>
            {Object.keys(profiles).map((name) => (
              <ProfileListRow key={name} theme={theme}
                name={name} isDefault={name === "default"}
                selected={selected === name}
                onClick={() => setSelected(name)} />
            ))}
          </div>
        </div>
        {/* Editor */}
        <ProfileEditor theme={theme} kind={tab}
          name={selected} profile={profiles[selected]}
          isDefault={selected === "default"} />
      </div>
    </div>
  );
}

function ProfileTab({ theme, active, onClick, label, sub }) {
  const t = APP_T[theme];
  return (
    <button onClick={onClick} style={{
      padding: "10px 16px 12px",
      background: "transparent", border: "none",
      borderBottom: `2px solid ${active ? t.accent : "transparent"}`,
      cursor: "pointer", fontFamily: "inherit", textAlign: "left",
      marginBottom: -1,
    }}>
      <div style={{ fontSize: 13, fontWeight: active ? 600 : 500, color: active ? t.text : t.textDim }}>{label}</div>
      <div style={{ fontSize: 11, color: t.textFaint, marginTop: 1 }}>{sub}</div>
    </button>
  );
}

function ProfileListRow({ theme, name, isDefault, selected, onClick }) {
  const t = APP_T[theme];
  const [hover, setHover] = React.useState(false);
  return (
    <div onClick={onClick}
      onMouseEnter={() => setHover(true)} onMouseLeave={() => setHover(false)}
      style={{
        padding: "10px 12px", display: "flex", alignItems: "center", gap: 8,
        background: selected ? t.rowSelected : (hover ? t.rowHover : "transparent"),
        borderLeft: `3px solid ${selected ? t.accent : "transparent"}`,
        borderBottom: `1px solid ${t.border}`,
        cursor: "pointer",
      }}>
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{
          fontSize: 13, fontWeight: selected ? 600 : 500,
          fontFamily: "ui-monospace, 'JetBrains Mono', Menlo, monospace",
          color: selected ? t.rowSelectedText : t.text,
        }}>{name}</div>
      </div>
      {isDefault && <AppChip theme={theme} tone="accent">default</AppChip>}
    </div>
  );
}

function ProfileEditor({ theme, kind, name, profile, isDefault }) {
  const t = APP_T[theme];
  const fields = kind === "system" ? SYSTEM_PROFILE_FIELDS : USER_PROFILE_FIELDS;
  const defaults = kind === "system" ? SAMPLE_SYSTEM_PROFILES.default : SAMPLE_USER_PROFILES.default;

  return (
    <div style={{ flex: 1, overflow: "auto", padding: "20px 28px", minWidth: 0 }}>
      <div style={{ display: "flex", alignItems: "flex-start", gap: 14, marginBottom: 18 }}>
        <div style={{ flex: 1 }}>
          <div style={{
            fontSize: 22, fontWeight: 700, color: t.text,
            fontFamily: "ui-monospace, 'JetBrains Mono', Menlo, monospace",
          }}>{name}</div>
          <div style={{ fontSize: 12.5, color: t.textDim, marginTop: 4 }}>
            {isDefault
              ? `The ${kind} default. Other ${kind} profiles inherit any unspecified fields from here.`
              : `Inherits from ${kind === "system" ? "system" : "user"}.profiles.default — unset fields fall through.`}
          </div>
        </div>
        {!isDefault && (
          <div style={{ display: "flex", gap: 8 }}>
            <AppBtn theme={theme}>Duplicate</AppBtn>
            <AppBtn theme={theme} variant="danger">Delete</AppBtn>
          </div>
        )}
      </div>

      {!isDefault && (
        <div style={{
          padding: "10px 14px", marginBottom: 16,
          background: t.accentSoft,
          border: `1px solid ${t.accent}`,
          borderRadius: 5,
          display: "flex", gap: 10, alignItems: "flex-start",
        }}>
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none" style={{ flexShrink: 0, marginTop: 1 }}>
            <circle cx="8" cy="8" r="7" stroke={t.accent} strokeWidth="1.5" />
            <path d="M8 7v4M8 5v.01" stroke={t.accent} strokeWidth="1.5" strokeLinecap="round" />
          </svg>
          <div style={{ fontSize: 12.5, color: t.text }}>
            <b>Inheritance:</b> blank rows fall through to <span style={{ fontFamily: "ui-monospace, monospace" }}>{kind}.profiles.default</span>.
            The grayed value next to each row is what you'd get if you cleared the override.
          </div>
        </div>
      )}

      <AppGroup theme={theme} title={kind === "system" ? "Lifecycle events" : "Session events"}>
        {fields.map((f, i) => (
          <ProfileFieldRow key={f.key} theme={theme}
            field={f}
            override={profile[f.key]}
            inherited={defaults[f.key]}
            isDefault={isDefault}
            isFirst={i === 0}
          />
        ))}
      </AppGroup>

      {kind === "system" && (
        <AppGroup theme={theme} title="Automation gate">
          <AppFormRow theme={theme} layout="horizontal" labelWidth={180}
            label="Automate when"
            helper="Combined with the TV's `managed` flag and current input.">
            <AppSelect theme={theme}
              value={profile.automate_when || defaults.automate_when}
              options={AUTOMATE_WHEN_OPTIONS} />
          </AppFormRow>
        </AppGroup>
      )}

      <AppGroup theme={theme} title="Used by">
        <UsedByList theme={theme} kind={kind} name={name} />
      </AppGroup>
    </div>
  );
}

function ProfileFieldRow({ theme, field, override, inherited, isDefault, isFirst }) {
  const t = APP_T[theme];
  const hasOverride = override !== undefined;
  const effective = hasOverride ? override : inherited;
  return (
    <div style={{
      paddingTop: isFirst ? 0 : 10, paddingBottom: 10,
      borderTop: isFirst ? "none" : `1px solid ${t.border}`,
      display: "flex", alignItems: "flex-start", gap: 14,
    }}>
      <div style={{ width: 200, flexShrink: 0, paddingTop: 5 }}>
        <div style={{ fontSize: 13, fontWeight: 500, color: t.text }}>{field.label}</div>
        <div style={{ fontSize: 11, color: t.textFaint, marginTop: 2, fontFamily: "ui-monospace, monospace" }}>{field.key}</div>
      </div>
      <div style={{ flex: 1, display: "flex", flexDirection: "column", gap: 4 }}>
        <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
          {field.numeric ? (
            <AppText theme={theme} value={String(effective ?? 0)} monospace />
          ) : (
            <div style={{ flex: 1, maxWidth: 280 }}>
              <AppSelect theme={theme} value={effective} options={ACTION_OPTIONS} />
            </div>
          )}
          {!isDefault && hasOverride && (
            <AppChip theme={theme} tone="accent">overridden</AppChip>
          )}
          {!isDefault && !hasOverride && (
            <span style={{ fontSize: 11, color: t.textFaint }}>
              ↳ inherits <code style={{ fontFamily: "ui-monospace, monospace", color: t.textDim }}>{String(inherited)}</code>
            </span>
          )}
        </div>
        <div style={{ fontSize: 11.5, color: t.textDim }}>{field.helper}</div>
      </div>
    </div>
  );
}

function UsedByList({ theme, kind, name }) {
  const t = APP_T[theme];
  const usage = {
    system: { default: ["Living Room G4", "Office QN90C"], guest: ["Guest Bravia"], shared: [] },
    user:   { default: ["Living Room G4", "Guest Bravia"], gaming: ["Office QN90C"], "movie-night": [] },
  };
  const tvs = usage[kind][name] || [];
  if (tvs.length === 0) {
    return <div style={{ fontSize: 12.5, color: t.textFaint, fontStyle: "italic" }}>Not assigned to any TV.</div>;
  }
  return (
    <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
      {tvs.map((n) => (
        <div key={n} style={{
          display: "inline-flex", alignItems: "center", gap: 6,
          padding: "5px 10px", borderRadius: 14,
          background: t.surface2, border: `1px solid ${t.border}`,
          fontSize: 12, color: t.text,
        }}>
          <span style={{ width: 6, height: 6, borderRadius: 3, background: t.success }} />
          {n}
        </div>
      ))}
    </div>
  );
}

// =========================================================================
// Session pane (placeholder)
// =========================================================================
function SessionPane({ theme }) {
  const t = APP_T[theme];
  return (
    <div style={{ flex: 1, display: "flex", flexDirection: "column", minWidth: 0, background: t.surface }}>
      <PaneHeader theme={theme} title="Session"
        subtitle="Per-user session-service controls." />
      <div style={{ flex: 1, display: "flex", alignItems: "center", justifyContent: "center", background: t.surfaceAlt, padding: 40 }}>
        <AppCard theme={theme} style={{ maxWidth: 380, padding: 28, textAlign: "center" }}>
          <div style={{ fontSize: 16, fontWeight: 600, color: t.text }}>Coming in M5</div>
          <div style={{ fontSize: 12.5, color: t.textDim, marginTop: 6 }}>
            Per-TV user-profile assignments, idle-timeout controls, and live session-event log.
          </div>
        </AppCard>
      </div>
    </div>
  );
}

// =========================================================================
// About pane
// =========================================================================
function AboutPane({ theme, daemonAvailable, sessionAvailable }) {
  const t = APP_T[theme];
  return (
    <div style={{ flex: 1, display: "flex", flexDirection: "column", minWidth: 0, background: t.surfaceAlt }}>
      <div style={{ flex: 1, overflow: "auto", padding: "32px 32px 24px" }}>
        <div style={{ display: "flex", alignItems: "center", gap: 16, marginBottom: 22 }}>
          <Logo size={56} />
          <div>
            <div style={{ fontSize: 28, fontWeight: 700, color: t.text }}>tisvlay</div>
            <div style={{ fontSize: 13, color: t.textDim, marginTop: 2 }}>
              Multi-vendor TV control for Linux desktops.
            </div>
          </div>
          <div style={{ flex: 1 }} />
          <AppChip theme={theme} tone="neutral">v0.5.0</AppChip>
        </div>

        <AppGroup theme={theme} title="Connections">
          <ConnectionRow theme={theme} ok={daemonAvailable}
            title="Daemon (system bus)"
            address="org.tisvlay.Daemon"
            description="Vendor-agnostic TV management, WoL at boot, per-TV D-Bus objects."
          />
          <div style={{ height: 1, background: t.border, margin: "12px 0" }} />
          <ConnectionRow theme={theme} ok={sessionAvailable}
            title="Session (user bus)"
            address="org.tisvlay.Session"
            description="User-profile assignments and session-event dispatch."
          />
        </AppGroup>

        <AppGroup theme={theme} title="Components">
          <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 12.5 }}>
            <tbody>
              {[
                ["tisvlayd", "system daemon · /var/lib/tisvlay/"],
                ["tisvlay-session", "user service · ~/.config/tisvlay/"],
                ["tisvlay", "command-line interface"],
                ["tisvlay-tray", "system tray (ksni)"],
                ["tisvlay-gui", "this window"],
              ].map(([k, v], i, arr) => (
                <tr key={k} style={{ borderBottom: i < arr.length - 1 ? `1px solid ${t.border}` : "none" }}>
                  <td style={{ width: 180, padding: "10px 0", fontFamily: "ui-monospace, monospace", color: t.text, fontSize: 12 }}>{k}</td>
                  <td style={{ padding: "10px 0", color: t.textDim }}>{v}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </AppGroup>

        <AppGroup theme={theme} title="License">
          <div style={{ display: "flex", alignItems: "center", gap: 14 }}>
            <div style={{ flex: 1, fontSize: 12.5, color: t.text }}>
              Apache-2.0 · © 2024–2026 tisvlay contributors
            </div>
            <AppBtn theme={theme}>View source</AppBtn>
            <AppBtn theme={theme}>Report issue</AppBtn>
          </div>
        </AppGroup>
      </div>
    </div>
  );
}

function ConnectionRow({ theme, ok, title, address, description }) {
  const t = APP_T[theme];
  return (
    <div style={{ display: "flex", alignItems: "flex-start", gap: 12 }}>
      <div style={{
        width: 28, height: 28, borderRadius: 14,
        background: ok ? t.successSoft : t.dangerSoft,
        color: ok ? t.success : t.danger,
        display: "flex", alignItems: "center", justifyContent: "center",
        flexShrink: 0,
      }}>
        {ok ? (
          <svg width="14" height="14" viewBox="0 0 12 12" fill="none">
            <path d="M2.5 6.2L4.8 8.5L9.5 3.5" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        ) : (
          <svg width="14" height="14" viewBox="0 0 12 12" fill="none">
            <path d="M3 3L9 9M9 3L3 9" stroke="currentColor" strokeWidth="2" strokeLinecap="round" />
          </svg>
        )}
      </div>
      <div style={{ flex: 1 }}>
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <div style={{ fontSize: 13, fontWeight: 600, color: t.text }}>{title}</div>
          <AppChip theme={theme} tone={ok ? "success" : "danger"}>{ok ? "online" : "offline"}</AppChip>
        </div>
        <div style={{ fontSize: 11.5, color: t.textFaint, fontFamily: "ui-monospace, monospace", marginTop: 2 }}>{address}</div>
        <div style={{ fontSize: 12, color: t.textDim, marginTop: 4 }}>{description}</div>
      </div>
    </div>
  );
}

Object.assign(window, {
  ProfilesPane, SessionPane, AboutPane,
  SAMPLE_SYSTEM_PROFILES, SAMPLE_USER_PROFILES,
});
