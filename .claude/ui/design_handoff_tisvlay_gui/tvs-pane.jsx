// TVs pane — full master/detail composition.

const SAMPLE_TVS_FULL = [
  {
    id: "living-room", name: "Living Room G4", vendor: "lg", connected: true,
    powerState: "on", isScreenOn: true, volume: 32, muted: false, backlight: 80,
    currentInput: "HDMI_1", availableInputs: ["HDMI_1", "HDMI_2", "HDMI_3", "Live TV"],
    systemProfile: "default", userProfile: "default", primaryInput: "HDMI_1",
    managed: true, automated: true,
    hwManufacturer: "LG Electronics", hwModel: "OLED65G4PUA", hwSerial: "412MAQS1B842",
    desktopConnector: "HDMI-A-1", ip: "192.168.1.100",
  },
  {
    id: "office", name: "Office QN90C", vendor: "samsung", connected: true,
    powerState: "on", isScreenOn: false, volume: 12, muted: true, backlight: 60,
    currentInput: "HDMI_2", availableInputs: ["HDMI_1", "HDMI_2", "HDMI_3"],
    systemProfile: "default", userProfile: "gaming", primaryInput: "HDMI_2",
    managed: true, automated: false,
    hwManufacturer: "Samsung", hwModel: "QN65QN90C", hwSerial: "08T7H4MR700123",
    desktopConnector: "DP-2", ip: "192.168.1.142",
  },
  {
    id: "guest", name: "Guest Bravia", vendor: "sony", connected: false,
    powerState: "standby", isScreenOn: false, volume: 0, muted: false, backlight: 0,
    currentInput: "", availableInputs: [],
    systemProfile: "guest", userProfile: "default", primaryInput: "HDMI_1",
    managed: false, automated: false,
    hwManufacturer: "Sony", hwModel: "XR-65A95L", hwSerial: "8024681357",
    desktopConnector: "—", ip: "192.168.1.187",
  },
];

// =========================================================================
// TVs list (master)
// =========================================================================
function TvList({ theme, tvs, selected, onSelect, onAdd }) {
  const t = APP_T[theme];
  return (
    <div style={{
      width: 260, flexShrink: 0,
      borderRight: `1px solid ${t.border}`,
      background: t.surfaceAlt,
      display: "flex", flexDirection: "column",
      minHeight: 0,
    }}>
      <div style={{
        padding: "10px 12px",
        display: "flex", alignItems: "center", gap: 8,
        borderBottom: `1px solid ${t.border}`,
        background: t.surface,
        flexShrink: 0,
      }}>
        <span style={{ fontSize: 11, fontWeight: 600, color: t.textDim, textTransform: "uppercase", letterSpacing: 0.6, flex: 1 }}>
          {tvs.length} configured
        </span>
        <IconBtn theme={theme} title="Pair a new TV" variant="accent" onClick={onAdd}>
          <SidebarIcon name="plus" size={14} />
        </IconBtn>
      </div>
      <div style={{ flex: 1, overflow: "auto" }}>
        {tvs.map((tv) => (
          <TvListRow
            key={tv.id} theme={theme} tv={tv}
            selected={selected === tv.id}
            onClick={() => onSelect && onSelect(tv.id)}
          />
        ))}
      </div>
    </div>
  );
}

function TvListRow({ theme, tv, selected, onClick }) {
  const t = APP_T[theme];
  const [hover, setHover] = React.useState(false);
  return (
    <div
      onClick={onClick}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      style={{
        padding: "11px 12px",
        display: "flex", alignItems: "center", gap: 10,
        background: selected ? t.rowSelected : (hover ? t.rowHover : "transparent"),
        borderLeft: `3px solid ${selected ? t.accent : "transparent"}`,
        cursor: "pointer",
        borderBottom: `1px solid ${t.border}`,
      }}
    >
      <AppVendorIcon theme={theme} vendor={tv.vendor} size={28} />
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{
          fontSize: 13, fontWeight: selected ? 600 : 500,
          color: selected ? t.rowSelectedText : t.text,
          overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap",
        }}>{tv.name}</div>
        <div style={{
          fontSize: 11, color: t.textDim, marginTop: 1,
          display: "flex", gap: 6, alignItems: "center",
        }}>
          <span style={{
            width: 6, height: 6, borderRadius: 3,
            background: tv.connected ? t.success : t.textFaint,
          }} />
          <span>{tv.connected ? (tv.isScreenOn ? "On" : "Screen off") : "Standby"}</span>
          {!tv.managed && <span style={{ color: t.warnText }}>· unmanaged</span>}
        </div>
      </div>
    </div>
  );
}

// =========================================================================
// TV detail
// =========================================================================
function TvDetail({ theme, tv, onChange }) {
  const t = APP_T[theme];
  if (!tv) return <TvDetailEmpty theme={theme} />;

  const set = (key, value) => onChange && onChange({ ...tv, [key]: value });

  return (
    <div style={{ flex: 1, overflow: "auto", background: t.surfaceAlt, minWidth: 0 }}>
      {/* Header */}
      <div style={{
        padding: "18px 24px 14px",
        background: t.surface,
        borderBottom: `1px solid ${t.border}`,
        display: "flex", alignItems: "center", gap: 14,
      }}>
        <AppVendorIcon theme={theme} vendor={tv.vendor} size={40} />
        <div style={{ flex: 1, minWidth: 0 }}>
          <div style={{ fontSize: 20, fontWeight: 700, color: t.text }}>{tv.name}</div>
          <div style={{
            fontSize: 12, color: t.textDim, marginTop: 4,
            display: "flex", gap: 10, alignItems: "center", flexWrap: "wrap",
          }}>
            <span style={{ fontFamily: "ui-monospace, monospace" }}>{tv.ip}</span>
            <span>·</span>
            <span>{tv.hwModel || "—"}</span>
          </div>
        </div>
        <div style={{ display: "flex", gap: 6 }}>
          {tv.connected
            ? <AppChip theme={theme} tone="success">Connected</AppChip>
            : <AppChip theme={theme} tone="neutral">Disconnected</AppChip>}
          {tv.managed
            ? (tv.automated
                ? <AppChip theme={theme} tone="accent">Automated</AppChip>
                : <AppChip theme={theme} tone="neutral">Managed</AppChip>)
            : <AppChip theme={theme} tone="warn">Unmanaged</AppChip>}
        </div>
      </div>

      <div style={{ padding: "20px 24px" }}>
        {/* Power */}
        <AppGroup theme={theme} title="Power">
          <div style={{ display: "flex", alignItems: "center", gap: 14 }}>
            <AppBtn theme={theme}
              variant={tv.connected ? "default" : "accent"}
              size="lg"
              onClick={() => set("connected", !tv.connected)}
            >
              {tv.connected ? "Power off" : "Power on"}
            </AppBtn>
            <div style={{ flex: 1, color: t.textDim, fontSize: 12 }}>
              {tv.connected
                ? `Power state: ${tv.powerState}`
                : "Wake this TV with WoL and reconnect."}
            </div>
          </div>
          <div style={{ height: 1, background: t.border, margin: "14px 0" }} />
          <div style={{ display: "flex", alignItems: "center", gap: 14 }}>
            <div style={{ width: 110, fontSize: 13, color: t.text }}>Screen</div>
            <AppSwitch theme={theme} checked={tv.isScreenOn}
              disabled={!tv.connected}
              onChange={(v) => set("isScreenOn", v)} />
            <div style={{ flex: 1, fontSize: 11.5, color: t.textFaint }}>
              Blanks the panel without powering off (OLED-safe, instant resume).
            </div>
          </div>
        </AppGroup>

        {/* Audio */}
        <AppGroup theme={theme} title="Audio">
          <div style={{ display: "flex", alignItems: "center", gap: 14, marginBottom: 12 }}>
            <div style={{ width: 110, fontSize: 13, color: t.text }}>Volume</div>
            <AppSlider theme={theme} value={tv.volume} disabled={!tv.connected || tv.muted}
              onChange={(v) => set("volume", v)} />
            <div style={{ width: 32, textAlign: "right", fontSize: 13, color: tv.connected ? t.text : t.textFaint, fontVariantNumeric: "tabular-nums" }}>
              {tv.connected ? tv.volume : "—"}
            </div>
          </div>
          <div style={{ display: "flex", alignItems: "center", gap: 14 }}>
            <div style={{ width: 110, fontSize: 13, color: t.text }}>Mute</div>
            <AppSwitch theme={theme} checked={tv.muted}
              disabled={!tv.connected} onChange={(v) => set("muted", v)} />
          </div>
        </AppGroup>

        {/* Picture */}
        <AppGroup theme={theme} title="Picture">
          <div style={{ display: "flex", alignItems: "center", gap: 14 }}>
            <div style={{ width: 110, fontSize: 13, color: t.text }}>Backlight</div>
            <AppSlider theme={theme} value={tv.backlight} disabled={!tv.connected}
              onChange={(v) => set("backlight", v)} />
            <div style={{ width: 32, textAlign: "right", fontSize: 13, color: tv.connected ? t.text : t.textFaint, fontVariantNumeric: "tabular-nums" }}>
              {tv.connected ? tv.backlight : "—"}
            </div>
          </div>
        </AppGroup>

        {/* Input */}
        <AppGroup theme={theme} title="Input">
          <div style={{ display: "flex", alignItems: "center", gap: 14 }}>
            <div style={{ width: 110, fontSize: 13, color: t.text }}>Current</div>
            <div style={{ flex: 1 }}>
              <AppSelect theme={theme}
                value={tv.currentInput || ""}
                disabled={!tv.connected || tv.availableInputs.length === 0}
                options={tv.availableInputs.length > 0
                  ? tv.availableInputs.map((i) => ({ value: i, label: i.replace("_", " ") }))
                  : [{ value: "", label: "No inputs available" }]}
                onChange={(v) => set("currentInput", v)} />
            </div>
          </div>
        </AppGroup>

        {/* Configuration */}
        <AppGroup theme={theme} title="Configuration">
          <AppFormRow theme={theme} layout="horizontal" labelWidth={140}
            label="Managed"
            helper="Participates in boot, sleep, and session automation.">
            <div style={{ display: "flex", alignItems: "center", gap: 10, height: 30 }}>
              <AppSwitch theme={theme} checked={tv.managed} onChange={(v) => set("managed", v)} />
              {tv.automated && <span style={{ fontSize: 11.5, color: t.accent, fontWeight: 500 }}>● automated now</span>}
            </div>
          </AppFormRow>
          <AppFormRow theme={theme} layout="horizontal" labelWidth={140}
            label="System profile"
            helper="Drives daemon lifecycle (boot/shutdown/sleep).">
            <AppSelect theme={theme} value={tv.systemProfile}
              options={[
                { value: "default", label: "default" },
                { value: "guest", label: "guest" },
                { value: "shared", label: "shared" },
              ]}
              onChange={(v) => set("systemProfile", v)} />
          </AppFormRow>
          <AppFormRow theme={theme} layout="horizontal" labelWidth={140}
            label="User profile"
            helper="Drives session events (lock/unlock/idle).">
            <AppSelect theme={theme} value={tv.userProfile}
              options={[
                { value: "default", label: "default" },
                { value: "gaming", label: "gaming" },
                { value: "movie-night", label: "movie-night" },
              ]}
              onChange={(v) => set("userProfile", v)} />
          </AppFormRow>
          <AppFormRow theme={theme} layout="horizontal" labelWidth={140}
            label="Primary input"
            helper="Which input this PC uses; affects on-primary automation.">
            <AppSelect theme={theme} value={tv.primaryInput}
              options={tv.availableInputs.length > 0
                ? tv.availableInputs.map((i) => ({ value: i, label: i.replace("_", " ") }))
                : [{ value: tv.primaryInput, label: tv.primaryInput }]}
              onChange={(v) => set("primaryInput", v)} />
          </AppFormRow>
        </AppGroup>

        {/* Hardware */}
        <AppGroup theme={theme} title="Hardware identity">
          <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 12.5 }}>
            <tbody>
              {[
                ["TV ID", tv.id, true],
                ["Manufacturer", tv.hwManufacturer || "—"],
                ["Model", tv.hwModel || "—"],
                ["Serial", tv.hwSerial || "—", true],
                ["DRM connector", tv.desktopConnector || "—", true],
              ].map(([k, v, mono]) => (
                <tr key={k}>
                  <td style={{ width: 140, padding: "6px 0", color: t.textDim }}>{k}</td>
                  <td style={{
                    padding: "6px 0", color: t.text,
                    fontFamily: mono ? "ui-monospace, monospace" : "inherit",
                  }}>{v}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </AppGroup>

        {/* Danger zone */}
        <AppGroup theme={theme} title="Danger zone">
          <div style={{ display: "flex", alignItems: "center", gap: 14 }}>
            <div style={{ flex: 1, fontSize: 12.5, color: t.textDim }}>
              Removes this TV from the daemon's configuration. Pairing must be redone.
            </div>
            <AppBtn theme={theme} variant="danger">Remove TV</AppBtn>
          </div>
        </AppGroup>
      </div>
    </div>
  );
}

function TvDetailEmpty({ theme, onAdd }) {
  const t = APP_T[theme];
  return (
    <div style={{
      flex: 1, background: t.surfaceAlt,
      display: "flex", alignItems: "center", justifyContent: "center",
      padding: 40,
    }}>
      <div style={{ textAlign: "center", maxWidth: 320 }}>
        <div style={{
          width: 56, height: 56, borderRadius: 28, margin: "0 auto 14px",
          background: t.accentSoft, color: t.accent,
          display: "flex", alignItems: "center", justifyContent: "center",
        }}>
          <SidebarIcon name="tv" size={28} />
        </div>
        <div style={{ fontSize: 15, fontWeight: 600, color: t.text }}>Select a TV</div>
        <div style={{ fontSize: 12.5, color: t.textDim, marginTop: 4 }}>
          Pick a TV from the list to control it, or add a new one with the + button.
        </div>
      </div>
    </div>
  );
}

// Empty (no TVs at all)
function TvsPaneEmpty({ theme, onAdd }) {
  const t = APP_T[theme];
  return (
    <div style={{
      flex: 1, background: t.surfaceAlt,
      display: "flex", alignItems: "center", justifyContent: "center",
      padding: 40,
    }}>
      <AppCard theme={theme} style={{
        maxWidth: 380, padding: 28, textAlign: "center",
      }}>
        <div style={{
          width: 64, height: 64, borderRadius: 32, margin: "0 auto 14px",
          background: t.accentSoft, color: t.accent,
          display: "flex", alignItems: "center", justifyContent: "center",
        }}>
          <svg width="32" height="32" viewBox="0 0 24 24" fill="none">
            <rect x="3" y="5" width="18" height="12" rx="1.5" stroke="currentColor" strokeWidth="1.6" />
            <path d="M8 21h8M12 17v4" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
          </svg>
        </div>
        <div style={{ fontSize: 17, fontWeight: 600, color: t.text }}>No TVs configured yet</div>
        <div style={{ fontSize: 13, color: t.textDim, marginTop: 6, marginBottom: 18 }}>
          Pair your first TV to control its power, screen, volume, inputs, and backlight from this window.
        </div>
        <AppBtn theme={theme} variant="accent" size="lg" onClick={onAdd}>
          Pair a new TV
        </AppBtn>
      </AppCard>
    </div>
  );
}

// Daemon offline
function TvsPaneOffline({ theme }) {
  const t = APP_T[theme];
  return (
    <div style={{
      flex: 1, background: t.surfaceAlt,
      display: "flex", alignItems: "center", justifyContent: "center",
      padding: 40,
    }}>
      <AppCard theme={theme} style={{ maxWidth: 380, padding: 28, textAlign: "center" }}>
        <div style={{
          width: 56, height: 56, borderRadius: 28, margin: "0 auto 14px",
          background: t.dangerSoft, color: t.danger,
          display: "flex", alignItems: "center", justifyContent: "center",
        }}>
          <svg width="28" height="28" viewBox="0 0 24 24" fill="none">
            <path d="M12 8v5M12 16v.01" stroke="currentColor" strokeWidth="2" strokeLinecap="round" />
            <circle cx="12" cy="12" r="9" stroke="currentColor" strokeWidth="1.8" />
          </svg>
        </div>
        <div style={{ fontSize: 16, fontWeight: 600, color: t.text }}>Daemon unreachable</div>
        <div style={{ fontSize: 12.5, color: t.textDim, marginTop: 6, marginBottom: 16 }}>
          tisvlay-gui couldn't reach <span style={{ fontFamily: "ui-monospace, monospace" }}>tisvlayd</span> on
          the system bus. The service may be stopped, or D-Bus policy is blocking access.
        </div>
        <div style={{ display: "flex", gap: 8, justifyContent: "center" }}>
          <AppBtn theme={theme} variant="accent">Retry connection</AppBtn>
          <AppBtn theme={theme}>View logs</AppBtn>
        </div>
        <div style={{
          marginTop: 18, padding: "8px 12px",
          background: t.surface2, border: `1px solid ${t.border}`, borderRadius: 4,
          fontFamily: "ui-monospace, monospace", fontSize: 11, color: t.textDim,
          textAlign: "left",
        }}>
          $ systemctl status tisvlayd
        </div>
      </AppCard>
    </div>
  );
}

// =========================================================================
// Top-level: TVs pane composition
// =========================================================================
function TvsPane({ theme, tvs, selectedId, onSelect, onTvChange, onAdd }) {
  const sel = tvs.find((tv) => tv.id === selectedId);
  if (tvs.length === 0) {
    return (
      <div style={{ flex: 1, display: "flex", minWidth: 0 }}>
        <TvsPaneEmpty theme={theme} onAdd={onAdd} />
      </div>
    );
  }
  return (
    <div style={{ flex: 1, display: "flex", minWidth: 0 }}>
      <TvList theme={theme} tvs={tvs} selected={selectedId} onSelect={onSelect} onAdd={onAdd} />
      <TvDetail theme={theme} tv={sel} onChange={onTvChange} />
    </div>
  );
}

Object.assign(window, {
  SAMPLE_TVS_FULL, TvsPane, TvList, TvDetail, TvDetailEmpty, TvsPaneEmpty, TvsPaneOffline,
});
