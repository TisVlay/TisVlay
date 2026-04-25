// Shared design system for the tisvlay GUI app.
// Reuses tokens from the wizard (T light/dark) plus app-shell primitives.

// ---- Tokens (light + dark) ------------------------------------------------
const APP_T = {
  light: {
    bg: "#fbfbfc",
    surface: "#ffffff",
    surface2: "#f4f5f7",
    surfaceAlt: "#f9fafb",
    sidebar: "#1b1e23",
    sidebarText: "#cfd4dc",
    sidebarTextDim: "#8a909a",
    sidebarSelected: "#3daee9",
    sidebarHover: "#262a33",
    border: "#dcdfe4",
    borderStrong: "#c2c7d0",
    text: "#1a1d22",
    textDim: "#5b626d",
    textFaint: "#8a909a",
    accent: "#3daee9",
    accentSoft: "#e3f3fc",
    success: "#10b981",
    successSoft: "#d1fae5",
    successText: "#047857",
    danger: "#dc2626",
    dangerSoft: "#fee2e2",
    dangerText: "#b91c1c",
    warn: "#d97706",
    warnSoft: "#fef3c7",
    warnText: "#92400e",
    chip: "#eef0f3",
    chipText: "#374151",
    chipBorder: "#dcdfe4",
    rowHover: "#f4f5f7",
    rowSelected: "#e3f3fc",
    rowSelectedText: "#0e6e9c",
    titlebar: "#eaecef",
    titlebarBorder: "#d2d6dc",
    shadow: "0 1px 2px rgba(0,0,0,.04), 0 8px 24px rgba(0,0,0,.06)",
    cardShadow: "0 1px 2px rgba(0,0,0,.05)",
    track: "#e1e4ea",
  },
  dark: {
    bg: "#1b1e23",
    surface: "#232730",
    surface2: "#2b2f39",
    surfaceAlt: "#262a33",
    sidebar: "#14171c",
    sidebarText: "#cfd4dc",
    sidebarTextDim: "#6f7682",
    sidebarSelected: "#3daee9",
    sidebarHover: "#1f232b",
    border: "#383d48",
    borderStrong: "#4a4f5b",
    text: "#eef0f3",
    textDim: "#a4abb6",
    textFaint: "#6f7682",
    accent: "#3daee9",
    accentSoft: "#1c3a4d",
    success: "#34d399",
    successSoft: "#1a3a32",
    successText: "#6ee7b7",
    danger: "#f87171",
    dangerSoft: "#3a1c1c",
    dangerText: "#fca5a5",
    warn: "#fbbf24",
    warnSoft: "#3a2e0e",
    warnText: "#fcd34d",
    chip: "#323744",
    chipText: "#cfd4dc",
    chipBorder: "#3f4452",
    rowHover: "#2b2f39",
    rowSelected: "#1c3a4d",
    rowSelectedText: "#a5d8f0",
    titlebar: "#1f232b",
    titlebarBorder: "#383d48",
    shadow: "0 1px 2px rgba(0,0,0,.4), 0 12px 32px rgba(0,0,0,.35)",
    cardShadow: "0 1px 2px rgba(0,0,0,.3)",
    track: "#383d48",
  },
};

const APP_FONT = 'system-ui, "Inter", "Noto Sans", "Segoe UI", Roboto, sans-serif';

// ---- App window chrome ----------------------------------------------------
function AppWindow({ theme, children, width = 900, height = 640, title = "tisvlay" }) {
  const t = APP_T[theme];
  return (
    <div style={{
      width, height,
      background: t.surface,
      color: t.text,
      borderRadius: 8,
      boxShadow: t.shadow,
      overflow: "hidden",
      fontFamily: APP_FONT,
      fontSize: 13,
      lineHeight: 1.45,
      display: "flex",
      flexDirection: "column",
      border: `1px solid ${t.border}`,
    }}>
      <div style={{
        height: 32, flexShrink: 0,
        background: t.titlebar,
        borderBottom: `1px solid ${t.titlebarBorder}`,
        display: "flex", alignItems: "center", padding: "0 10px", gap: 8,
      }}>
        <div style={{ display: "flex", gap: 6 }}>
          <div style={{ width: 12, height: 12, borderRadius: 6, background: "#e0635a" }} />
          <div style={{ width: 12, height: 12, borderRadius: 6, background: "#e0a23a" }} />
          <div style={{ width: 12, height: 12, borderRadius: 6, background: "#5cb85c" }} />
        </div>
        <div style={{ flex: 1, textAlign: "center", fontSize: 12, color: t.textDim }}>{title}</div>
        <div style={{ width: 42 }} />
      </div>
      <div style={{ flex: 1, display: "flex", minHeight: 0 }}>
        {children}
      </div>
    </div>
  );
}

// ---- Sidebar --------------------------------------------------------------
function Sidebar({ theme, active, onSelect, daemonAvailable = true, sessionAvailable = true }) {
  const t = APP_T[theme];
  const items = [
    { id: "tvs",      label: "TVs",      icon: "tv" },
    { id: "profiles", label: "Profiles", icon: "profile" },
    { id: "session",  label: "Session",  icon: "session" },
    { id: "about",    label: "About",    icon: "about" },
  ];
  return (
    <div style={{
      width: 200, flexShrink: 0,
      background: t.sidebar,
      display: "flex", flexDirection: "column",
      padding: "12px 8px",
      gap: 2,
    }}>
      <div style={{
        padding: "6px 12px 14px", display: "flex", alignItems: "center", gap: 8,
      }}>
        <Logo size={22} />
        <span style={{ color: "#fff", fontSize: 16, fontWeight: 700, letterSpacing: 0.2 }}>tisvlay</span>
      </div>
      {items.map((it) => (
        <SidebarItem
          key={it.id}
          theme={theme}
          icon={it.icon}
          label={it.label}
          selected={active === it.id}
          onClick={() => onSelect && onSelect(it.id)}
        />
      ))}
      <div style={{ flex: 1 }} />
      <div style={{ padding: "8px 12px", display: "flex", flexDirection: "column", gap: 6 }}>
        <StatusLine theme={theme} ok={daemonAvailable} label="Daemon" />
        <StatusLine theme={theme} ok={sessionAvailable} label="Session" />
      </div>
    </div>
  );
}

function SidebarItem({ theme, icon, label, selected, onClick }) {
  const t = APP_T[theme];
  return (
    <div
      onClick={onClick}
      role="button"
      tabIndex={0}
      style={{
        height: 34, padding: "0 12px",
        borderRadius: 5,
        display: "flex", alignItems: "center", gap: 10,
        background: selected ? t.sidebarSelected : "transparent",
        color: selected ? "#fff" : t.sidebarText,
        fontSize: 13, fontWeight: selected ? 600 : 500,
        cursor: "pointer",
        transition: "background .12s",
      }}
      onMouseEnter={(e) => { if (!selected) e.currentTarget.style.background = t.sidebarHover; }}
      onMouseLeave={(e) => { if (!selected) e.currentTarget.style.background = "transparent"; }}
    >
      <SidebarIcon name={icon} />
      <span>{label}</span>
    </div>
  );
}

function SidebarIcon({ name, size = 16 }) {
  const stroke = "currentColor";
  const sw = 1.6;
  switch (name) {
    case "tv":
      return (
        <svg width={size} height={size} viewBox="0 0 16 16" fill="none">
          <rect x="1.5" y="3" width="13" height="8.5" rx="1" stroke={stroke} strokeWidth={sw} />
          <path d="M5.5 14h5M8 11.5V14" stroke={stroke} strokeWidth={sw} strokeLinecap="round" />
        </svg>
      );
    case "profile":
      return (
        <svg width={size} height={size} viewBox="0 0 16 16" fill="none">
          <rect x="2" y="3" width="12" height="10" rx="1.5" stroke={stroke} strokeWidth={sw} />
          <path d="M5 6.5h6M5 9h4" stroke={stroke} strokeWidth={sw} strokeLinecap="round" />
        </svg>
      );
    case "session":
      return (
        <svg width={size} height={size} viewBox="0 0 16 16" fill="none">
          <circle cx="8" cy="8" r="6" stroke={stroke} strokeWidth={sw} />
          <path d="M8 4.5v3.7l2.3 1.4" stroke={stroke} strokeWidth={sw} strokeLinecap="round" />
        </svg>
      );
    case "about":
      return (
        <svg width={size} height={size} viewBox="0 0 16 16" fill="none">
          <circle cx="8" cy="8" r="6" stroke={stroke} strokeWidth={sw} />
          <path d="M8 7v4M8 5v.01" stroke={stroke} strokeWidth={sw} strokeLinecap="round" />
        </svg>
      );
    case "plus":
      return (
        <svg width={size} height={size} viewBox="0 0 16 16" fill="none">
          <path d="M8 3v10M3 8h10" stroke={stroke} strokeWidth={sw} strokeLinecap="round" />
        </svg>
      );
    default: return null;
  }
}

function Logo({ size = 22 }) {
  return (
    <div style={{
      width: size, height: size, borderRadius: 5,
      background: "linear-gradient(135deg, #3daee9 0%, #2080ba 100%)",
      display: "flex", alignItems: "center", justifyContent: "center",
      color: "white", fontWeight: 800, fontSize: size * 0.55, letterSpacing: -0.3,
    }}>t</div>
  );
}

function StatusLine({ theme, ok, label }) {
  const t = APP_T[theme];
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 11 }}>
      <span style={{
        width: 7, height: 7, borderRadius: 4,
        background: ok ? t.success : t.danger,
      }} />
      <span style={{ color: ok ? t.sidebarText : t.danger }}>{label}</span>
      <span style={{ color: t.sidebarTextDim, marginLeft: "auto" }}>
        {ok ? "online" : "offline"}
      </span>
    </div>
  );
}

// ---- Pane header ----------------------------------------------------------
function PaneHeader({ theme, title, subtitle, actions }) {
  const t = APP_T[theme];
  return (
    <div style={{
      padding: "16px 24px 14px",
      borderBottom: `1px solid ${t.border}`,
      display: "flex", alignItems: "center", gap: 14,
      background: t.surface,
      flexShrink: 0,
    }}>
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ fontSize: 18, fontWeight: 600, color: t.text }}>{title}</div>
        {subtitle && <div style={{ fontSize: 12.5, color: t.textDim, marginTop: 2 }}>{subtitle}</div>}
      </div>
      {actions}
    </div>
  );
}

// ---- Buttons --------------------------------------------------------------
function AppBtn({ theme, children, onClick, variant = "default", disabled, size = "md", iconLeft, title }) {
  const t = APP_T[theme];
  const isPrimary = variant === "accent";
  const isDanger = variant === "danger";
  const isGhost = variant === "ghost";
  const bg = disabled ? (theme === "light" ? "#eceef1" : "#2b2f39")
    : isPrimary ? t.accent
    : isDanger ? t.danger
    : isGhost ? "transparent"
    : t.surface;
  const fg = disabled ? t.textFaint
    : isPrimary || isDanger ? "#fff"
    : t.text;
  const border = disabled ? t.border
    : isPrimary ? t.accent
    : isDanger ? t.danger
    : isGhost ? "transparent"
    : t.borderStrong;
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      title={title}
      style={{
        display: "inline-flex", alignItems: "center", justifyContent: "center", gap: 6,
        height: size === "sm" ? 26 : size === "lg" ? 34 : 30,
        padding: size === "sm" ? "0 10px" : "0 14px",
        borderRadius: 4,
        background: bg, color: fg, border: `1px solid ${border}`,
        fontSize: size === "sm" ? 12 : 12.5,
        fontWeight: isPrimary ? 600 : 500,
        cursor: disabled ? "not-allowed" : "pointer",
        fontFamily: "inherit", outline: "none",
        whiteSpace: "nowrap",
      }}
    >
      {iconLeft}
      {children}
    </button>
  );
}

function IconBtn({ theme, children, onClick, title, variant = "default" }) {
  const t = APP_T[theme];
  return (
    <button
      onClick={onClick}
      title={title}
      style={{
        width: 30, height: 30, borderRadius: 4,
        display: "inline-flex", alignItems: "center", justifyContent: "center",
        background: variant === "accent" ? t.accent : t.surface,
        color: variant === "accent" ? "#fff" : t.text,
        border: `1px solid ${variant === "accent" ? t.accent : t.borderStrong}`,
        cursor: "pointer", fontFamily: "inherit",
      }}
    >{children}</button>
  );
}

// ---- Form primitives ------------------------------------------------------
function AppFormRow({ theme, label, helper, children, error, layout = "vertical", labelWidth = 160 }) {
  const t = APP_T[theme];
  if (layout === "horizontal") {
    return (
      <div style={{ display: "flex", alignItems: "flex-start", gap: 12, padding: "8px 0" }}>
        <label style={{ width: labelWidth, fontSize: 12.5, color: t.text, flexShrink: 0, paddingTop: 7 }}>
          {label}
        </label>
        <div style={{ flex: 1, display: "flex", flexDirection: "column", gap: 4 }}>
          {children}
          {error && <div style={{ fontSize: 11.5, color: t.danger }}>{error}</div>}
          {helper && !error && <div style={{ fontSize: 11.5, color: t.textDim }}>{helper}</div>}
        </div>
      </div>
    );
  }
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
      <label style={{ fontSize: 12, fontWeight: 500, color: t.text }}>{label}</label>
      {children}
      {error && <div style={{ fontSize: 11.5, color: t.danger }}>{error}</div>}
      {helper && !error && <div style={{ fontSize: 11.5, color: t.textDim }}>{helper}</div>}
    </div>
  );
}

function AppText({ theme, value, placeholder, readOnly, monospace, onChange }) {
  const t = APP_T[theme];
  return (
    <input
      type="text"
      value={value || ""}
      placeholder={placeholder}
      readOnly={readOnly}
      onChange={onChange ? (e) => onChange(e.target.value) : undefined}
      style={{
        height: 30, padding: "0 10px",
        borderRadius: 4,
        border: `1px solid ${t.borderStrong}`,
        background: readOnly ? t.surface2 : t.surface,
        color: t.text, fontSize: 13,
        fontFamily: monospace ? "ui-monospace, 'JetBrains Mono', Menlo, monospace" : "inherit",
        outline: "none", width: "100%",
      }}
    />
  );
}

function AppSelect({ theme, value, options, onChange, disabled }) {
  const t = APP_T[theme];
  return (
    <div style={{ position: "relative" }}>
      <select
        value={value} disabled={disabled}
        onChange={onChange ? (e) => onChange(e.target.value) : undefined}
        style={{
          width: "100%", height: 30, padding: "0 28px 0 10px",
          borderRadius: 4,
          border: `1px solid ${t.borderStrong}`,
          background: disabled ? t.surface2 : t.surface,
          color: disabled ? t.textFaint : t.text,
          fontSize: 13, fontFamily: "inherit", appearance: "none", outline: "none",
        }}
      >
        {options.map((o) => (
          <option key={o.value} value={o.value}>{o.label}</option>
        ))}
      </select>
      <svg width="10" height="6" viewBox="0 0 10 6" fill="none"
        style={{ position: "absolute", right: 10, top: "50%", transform: "translateY(-50%)", pointerEvents: "none" }}>
        <path d="M1 1L5 5L9 1" stroke={t.textDim} strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" />
      </svg>
    </div>
  );
}

function AppSwitch({ theme, checked, onChange, disabled }) {
  const t = APP_T[theme];
  return (
    <button
      onClick={() => !disabled && onChange && onChange(!checked)}
      disabled={disabled}
      style={{
        width: 36, height: 20, borderRadius: 10, padding: 0,
        background: checked ? t.accent : (theme === "light" ? "#cbd0d8" : "#4a4f5b"),
        border: "none", cursor: disabled ? "not-allowed" : "pointer",
        position: "relative", flexShrink: 0,
        opacity: disabled ? 0.5 : 1,
        transition: "background .12s",
      }}
    >
      <span style={{
        position: "absolute", top: 2, left: checked ? 18 : 2,
        width: 16, height: 16, borderRadius: 8,
        background: "#fff",
        boxShadow: "0 1px 3px rgba(0,0,0,.2)",
        transition: "left .14s",
      }} />
    </button>
  );
}

function AppSlider({ theme, value, min = 0, max = 100, onChange, disabled }) {
  const t = APP_T[theme];
  const pct = ((value - min) / (max - min)) * 100;
  return (
    <div style={{ position: "relative", height: 20, display: "flex", alignItems: "center", flex: 1 }}>
      <div style={{
        position: "absolute", left: 0, right: 0, height: 4,
        background: t.track, borderRadius: 2,
      }} />
      <div style={{
        position: "absolute", left: 0, width: `${pct}%`, height: 4,
        background: disabled ? t.textFaint : t.accent, borderRadius: 2,
      }} />
      <div style={{
        position: "absolute", left: `calc(${pct}% - 7px)`,
        width: 14, height: 14, borderRadius: 7,
        background: "#fff",
        border: `1.5px solid ${disabled ? t.textFaint : t.accent}`,
        boxShadow: "0 1px 3px rgba(0,0,0,.2)",
      }} />
    </div>
  );
}

function AppCheckbox({ theme, checked, onChange, label, helper, disabled }) {
  const t = APP_T[theme];
  return (
    <label style={{
      display: "flex", gap: 8, alignItems: "flex-start", cursor: disabled ? "not-allowed" : "pointer",
      opacity: disabled ? 0.6 : 1,
    }}>
      <span style={{
        width: 16, height: 16, borderRadius: 3, marginTop: 1, flexShrink: 0,
        background: checked ? t.accent : t.surface,
        border: `1px solid ${checked ? t.accent : t.borderStrong}`,
        display: "flex", alignItems: "center", justifyContent: "center",
      }}>
        {checked && (
          <svg width="11" height="11" viewBox="0 0 12 12" fill="none">
            <path d="M2.5 6.2L4.8 8.5L9.5 3.5" stroke="white" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        )}
      </span>
      <span>
        <span style={{ fontSize: 13, color: t.text }}>{label}</span>
        {helper && <div style={{ fontSize: 11.5, color: t.textDim, marginTop: 2 }}>{helper}</div>}
      </span>
      <input type="checkbox" checked={checked}
        onChange={(e) => onChange && onChange(e.target.checked)}
        disabled={disabled}
        style={{ display: "none" }} />
    </label>
  );
}

function AppChip({ theme, children, tone = "neutral", icon }) {
  const t = APP_T[theme];
  const tones = {
    neutral: { bg: t.chip, fg: t.chipText, br: t.chipBorder },
    success: { bg: t.successSoft, fg: t.successText, br: t.success },
    danger:  { bg: t.dangerSoft, fg: t.dangerText, br: t.danger },
    warn:    { bg: t.warnSoft, fg: t.warnText, br: t.warn },
    accent:  { bg: t.accentSoft, fg: t.accent, br: t.accent },
  };
  const c = tones[tone];
  return (
    <span style={{
      display: "inline-flex", alignItems: "center", gap: 5,
      padding: "3px 8px", borderRadius: 10,
      background: c.bg, color: c.fg,
      fontSize: 11, fontWeight: 600,
      border: `1px solid ${c.br}`,
      whiteSpace: "nowrap",
    }}>
      {icon}
      {children}
    </span>
  );
}

// ---- Card / Section -------------------------------------------------------
function AppCard({ theme, children, style, padded = true }) {
  const t = APP_T[theme];
  return (
    <div style={{
      background: t.surface,
      border: `1px solid ${t.border}`,
      borderRadius: 6,
      boxShadow: t.cardShadow,
      ...(padded ? { padding: 16 } : {}),
      ...style,
    }}>
      {children}
    </div>
  );
}

function AppGroup({ theme, title, action, children, style }) {
  const t = APP_T[theme];
  return (
    <section style={{ marginBottom: 16, ...style }}>
      <div style={{
        display: "flex", alignItems: "center", justifyContent: "space-between",
        marginBottom: 8, padding: "0 2px",
      }}>
        <h3 style={{
          margin: 0, fontSize: 11, fontWeight: 600,
          color: t.textDim, textTransform: "uppercase", letterSpacing: 0.6,
        }}>{title}</h3>
        {action}
      </div>
      <AppCard theme={theme} padded={false}>
        <div style={{ padding: 14 }}>{children}</div>
      </AppCard>
    </section>
  );
}

// ---- Vendor icon (mini) ---------------------------------------------------
function AppVendorIcon({ vendor, size = 24, theme }) {
  const t = APP_T[theme];
  const map = {
    lg:      { letters: "LG", bg: "#a50034", fg: "#fff" },
    samsung: { letters: "SS", bg: "#1428a0", fg: "#fff" },
    sony:    { letters: "SY", bg: "#111111", fg: "#fff" },
    mock:    { letters: "MK", bg: t.chip,    fg: t.chipText },
  };
  const v = map[vendor] || map.mock;
  return (
    <div style={{
      width: size, height: size, borderRadius: 5,
      background: v.bg, color: v.fg,
      display: "flex", alignItems: "center", justifyContent: "center",
      fontSize: size * 0.40, fontWeight: 700, letterSpacing: 0.3,
      flexShrink: 0,
      border: vendor === "mock" ? `1px solid ${t.border}` : "none",
    }}>{v.letters}</div>
  );
}

Object.assign(window, {
  APP_T, APP_FONT, AppWindow, Sidebar, SidebarItem, SidebarIcon, Logo,
  PaneHeader, AppBtn, IconBtn,
  AppFormRow, AppText, AppSelect, AppSwitch, AppSlider, AppCheckbox, AppChip,
  AppCard, AppGroup, AppVendorIcon, StatusLine,
});
