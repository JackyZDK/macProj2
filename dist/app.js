// IP 切换器前端逻辑：直接调用 Tauri 自定义命令，不做任何 mock。
const { invoke } = window.__TAURI__.core;

let interfaces = [];
let selectedService = null;
let presets = [];
let selectedPresetName = null;

// DOM 引用
const $ = (id) => document.getElementById(id);
const ifaceList = $("iface-list");
const ifaceDetail = $("iface-detail");
const detailService = $("detail-service");
const presetList = $("preset-list");
const statusBar = $("status-bar");
const applyIpBtn = $("apply-ip-btn");
const applyDhcpBtn = $("apply-dhcp-btn");
const busyOverlay = $("busy-overlay");
const busyText = $("busy-text");

// ---------------------------------------------------------------------------
// 状态栏 / 忙碌遮罩
// ---------------------------------------------------------------------------
function setStatus(msg, kind) {
  statusBar.textContent = msg;
  statusBar.classList.remove("error", "ok");
  if (kind) statusBar.classList.add(kind);
}

function setBusy(on, text) {
  busyOverlay.classList.toggle("hidden", !on);
  if (text) busyText.textContent = text;
  applyIpBtn.disabled = on;
  applyDhcpBtn.disabled = on;
}

// ---------------------------------------------------------------------------
// 网卡列表与详情
// ---------------------------------------------------------------------------
async function refreshInterfaces() {
  setBusy(true, "正在读取网卡…");
  try {
    interfaces = await invoke("list_interfaces");
    if (selectedService && !interfaces.some((i) => i.service === selectedService)) {
      selectedService = null;
    }
    if (!selectedService && interfaces.length > 0) {
      selectedService = interfaces[0].service;
    }
    renderIfaces();
    renderDetail(selectedService ? interfaces.find((i) => i.service === selectedService) : null);
    setStatus(interfaces.length ? `已发现 ${interfaces.length} 张物理网卡` : "未发现物理网卡（en* 设备）", interfaces.length ? undefined : "error");
    return true;
  } catch (err) {
    setStatus(`读取网卡失败：${err}`, "error");
    ifaceList.innerHTML = "";
    renderDetail(null);
    return false;
  } finally {
    setBusy(false);
  }
}

function renderIfaces() {
  if (interfaces.length === 0) {
    ifaceList.innerHTML = '<li class="empty">未发现物理网卡</li>';
    return;
  }
  ifaceList.innerHTML = "";
  for (const iface of interfaces) {
    const li = document.createElement("li");
    li.className = iface.service === selectedService ? "active" : "";

    const name = document.createElement("div");
    name.className = "name";
    name.textContent = iface.hardware_port || iface.service;

    const sub = document.createElement("div");
    sub.className = "sub";
    sub.textContent = `设备 ${iface.device}`;

    const ip = document.createElement("div");
    ip.className = "ip";
    ip.textContent = iface.ip || "未获取到 IP";

    const mode = document.createElement("span");
    mode.className = `mode ${iface.config_mode.toLowerCase() || "unknown"}`;
    mode.textContent = iface.config_mode || "未知";

    li.append(name, sub, ip, mode);
    li.addEventListener("click", () => selectIface(iface.service));
    ifaceList.appendChild(li);
  }
}

function selectIface(service) {
  if (selectedService === service) return;
  selectedService = service;
  trafficPrev = null; // 切换网卡时重置速率基线
  renderIfaces();
  renderDetail(interfaces.find((i) => i.service === service));
}

function renderDetail(iface) {
  if (!iface) {
    detailService.textContent = "";
    ifaceDetail.innerHTML = "";
    applyIpBtn.disabled = true;
    applyDhcpBtn.disabled = true;
    return;
  }
  detailService.textContent = `（${iface.service}）`;
  const statusText = { active: "已连接", inactive: "未连接" }[iface.status] || iface.status || "";
  const rows = [
    ["硬件端口", iface.hardware_port],
    ["设备", iface.device],
    ["配置方式", iface.config_mode],
    ["接口状态", statusText],
    ["IP 地址", iface.ip],
    ["子网掩码", iface.netmask],
    ["网关", iface.gateway],
    ["DNS", iface.dns.join(", ")],
    ["MAC 地址", iface.mac],
    ["MTU", iface.mtu],
    ["IPv6 配置", iface.ipv6_mode],
    ["IPv6 地址", iface.ipv6],
    ["IPv6 网关", iface.ipv6_router],
  ];
  ifaceDetail.innerHTML = "";
  for (const [label, value] of rows) {
    const dt = document.createElement("dt");
    dt.textContent = label;
    const dd = document.createElement("dd");
    dd.textContent = value || "—";
    if (!value) dd.classList.add("empty");
    ifaceDetail.append(dt, dd);
  }
  updateActionButtons();
}

// ---------------------------------------------------------------------------
// 预设
// ---------------------------------------------------------------------------
async function refreshPresets() {
  try {
    presets = await invoke("load_presets");
    if (selectedPresetName && !presets.some((p) => p.name === selectedPresetName)) {
      selectedPresetName = null;
    }
    renderPresets();
  } catch (err) {
    setStatus(`读取预设失败：${err}`, "error");
  }
  updateActionButtons();
}

function renderPresets() {
  if (presets.length === 0) {
    presetList.innerHTML = '<li class="empty">暂无预设，点击「新增预设」创建</li>';
    return;
  }
  presetList.innerHTML = "";
  for (const preset of presets) {
    const li = document.createElement("li");
    li.className = preset.name === selectedPresetName ? "active" : "";

    const info = document.createElement("div");
    info.className = "p-info";
    const name = document.createElement("div");
    name.className = "p-name";
    name.textContent = preset.name;
    const sub = document.createElement("div");
    sub.className = "p-sub";
    sub.textContent = `${preset.ip} / ${preset.netmask} / 网关 ${preset.gateway} / DNS ${preset.dns.join(",")}`;
    info.append(name, sub);

    const del = document.createElement("button");
    del.className = "p-del";
    del.textContent = "删除";
    del.title = "删除预设";
    del.addEventListener("click", (e) => {
      e.stopPropagation();
      deletePreset(preset.name);
    });

    li.append(info, del);
    li.addEventListener("click", () => {
      selectedPresetName = selectedPresetName === preset.name ? null : preset.name;
      renderPresets();
      updateActionButtons();
    });
    presetList.appendChild(li);
  }
}

async function deletePreset(name) {
  if (!confirm(`确定删除预设「${name}」？`)) return;
  presets = presets.filter((p) => p.name !== name);
  if (selectedPresetName === name) selectedPresetName = null;
  try {
    await invoke("save_presets", { presets });
    renderPresets();
    updateActionButtons();
    setStatus(`已删除预设「${name}」`);
    recordLog("delete_preset", `名称「${name}」`, "success", "预设已删除");
  } catch (err) {
    // 保存失败：回滚本地状态并重读磁盘
    await refreshPresets();
    setStatus(`删除预设失败：${err}`, "error");
    recordLog("delete_preset", `名称「${name}」`, "fail", String(err));
  }
}

// ---------------------------------------------------------------------------
// 流量统计
// ---------------------------------------------------------------------------
function formatBytes(n) {
  if (n === 0) return "0 B";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(2)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

let trafficPrev = null; // { device, rxBytes, txBytes, at }

function renderTraffic(stats) {
  const els = {
    "t-rx-rate": stats ? `${formatBytes(stats.rxRate)}/s` : "—",
    "t-tx-rate": stats ? `${formatBytes(stats.txRate)}/s` : "—",
    "t-rx-total": stats ? formatBytes(stats.rxBytes) : "—",
    "t-tx-total": stats ? formatBytes(stats.txBytes) : "—",
  };
  for (const [id, text] of Object.entries(els)) {
    $(id).textContent = text;
  }
}

async function refreshTraffic() {
  const iface = interfaces.find((i) => i.service === selectedService);
  if (!iface) {
    trafficPrev = null;
    renderTraffic(null);
    return;
  }
  try {
    const t = await invoke("get_traffic", { device: iface.device });
    const now = Date.now();
    let rxRate = 0;
    let txRate = 0;
    if (trafficPrev && trafficPrev.device === iface.device) {
      const dt = (now - trafficPrev.at) / 1000;
      if (dt >= 1) {
        rxRate = Math.max(0, t.rx_bytes - trafficPrev.rxBytes) / dt;
        txRate = Math.max(0, t.tx_bytes - trafficPrev.txBytes) / dt;
      }
    }
    trafficPrev = { device: iface.device, rxBytes: t.rx_bytes, txBytes: t.tx_bytes, at: now };
    renderTraffic({ rxBytes: t.rx_bytes, txBytes: t.tx_bytes, rxRate, txRate });
  } catch (_) {
    trafficPrev = null;
    renderTraffic(null);
  }
}

// 每 2 秒刷新一次流量；跳过繁忙状态避免叠加
setInterval(refreshTraffic, 2000);
refreshTraffic();

// ---------------------------------------------------------------------------
// 日志
// ---------------------------------------------------------------------------
function recordLog(action, detail, result, message) {
  invoke("log_action", { action, detail, result, message }).catch(() => {
    // 日志写入失败不影响主流程
  });
}

const ACTION_LABELS = {
  set_static_ip: "设置 IP",
  set_dhcp: "切换 DHCP",
  add_preset: "新增预设",
  delete_preset: "删除预设",
  refresh_interfaces: "刷新网卡",
};

const logsModal = $("logs-modal");
const logsList = $("logs-list");

async function openLogs() {
  logsModal.hidden = false;
  logsList.innerHTML = '<li class="empty">加载中…</li>';
  try {
    const logs = await invoke("list_logs");
    renderLogs(logs);
  } catch (err) {
    logsList.innerHTML = `<li class="empty">读取日志失败：${err}</li>`;
  }
}

function closeLogs() {
  logsModal.hidden = true;
}

function renderLogs(logs) {
  if (logs.length === 0) {
    logsList.innerHTML = '<li class="empty">暂无日志</li>';
    return;
  }
  logsList.innerHTML = "";
  for (const log of logs) {
    const li = document.createElement("li");

    const head = document.createElement("div");
    head.className = "log-item-head";
    const act = document.createElement("span");
    act.className = "log-action";
    act.textContent = ACTION_LABELS[log.action] || log.action;
    const time = document.createElement("span");
    time.className = "log-time";
    time.textContent = log.time;
    const result = document.createElement("span");
    result.className = `log-result ${log.result === "success" ? "success" : "fail"}`;
    result.textContent = log.result === "success" ? "成功" : "失败";
    head.append(act, time, result);

    const detail = document.createElement("div");
    detail.className = "log-detail";
    detail.textContent = log.detail;

    const msg = document.createElement("div");
    msg.className = "log-message";
    msg.textContent = log.message;

    li.append(head, detail, msg);
    logsList.appendChild(li);
  }
}

$("logs-btn").addEventListener("click", openLogs);
$("close-logs-btn").addEventListener("click", closeLogs);

// ---------------------------------------------------------------------------
// 预设表单（新增，模态弹窗）
// ---------------------------------------------------------------------------
const presetModal = $("preset-modal");

function openPresetModal() {
  presetModal.hidden = false;
  ["p-name", "p-ip", "p-netmask", "p-gateway", "p-dns"].forEach((id) => { $(id).value = ""; });
  $("p-name").focus();
}

function closePresetModal() {
  presetModal.hidden = true;
}

$("add-preset-btn").addEventListener("click", () => {
  $("preset-form-title").textContent = "新增预设";
  openPresetModal();
});

$("cancel-preset-btn").addEventListener("click", closePresetModal);
$("close-modal-btn").addEventListener("click", closePresetModal);

$("preset-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const name = $("p-name").value.trim();
  const ip = $("p-ip").value.trim();
  const netmask = $("p-netmask").value.trim();
  const gateway = $("p-gateway").value.trim();
  const dnsRaw = $("p-dns").value.trim();

  if (!name || !ip || !netmask || !gateway || !dnsRaw) {
    setStatus("请完整填写预设名称/IP/子网掩码/网关/DNS", "error");
    return;
  }
  const dns = dnsRaw.split(",").map((s) => s.trim()).filter(Boolean);
  if (presets.some((p) => p.name === name)) {
    setStatus(`预设名称「${name}」已存在`, "error");
    return;
  }

  presets.push({ name, ip, netmask, gateway, dns });
  selectedPresetName = name;
  try {
    await invoke("save_presets", { presets });
    closePresetModal();
    renderPresets();
    updateActionButtons();
    setStatus(`已保存预设「${name}」`);
    recordLog("add_preset", `名称「${name}」IP=${ip} 掩码=${netmask} 网关=${gateway} DNS=${dns.join("、")}`, "success", "预设已保存");
  } catch (err) {
    presets = presets.filter((p) => p.name !== name);
    setStatus(`保存预设失败：${err}`, "error");
    recordLog("add_preset", `名称「${name}」`, "fail", String(err));
  }
});

// ---------------------------------------------------------------------------
// 操作：设置 IP / 切换 DHCP
// ---------------------------------------------------------------------------
function currentPreset() {
  return presets.find((p) => p.name === selectedPresetName) || null;
}

function updateActionButtons() {
  const hasIface = Boolean(selectedService);
  applyIpBtn.disabled = !hasIface || !currentPreset();
  applyDhcpBtn.disabled = !hasIface;
}

applyIpBtn.addEventListener("click", async () => {
  const preset = currentPreset();
  if (!selectedService || !preset) return;
  if (
    !confirm(
      `确定将「${selectedService}」设置为：\nIP ${preset.ip} / 掩码 ${preset.netmask} / 网关 ${preset.gateway}\nDNS：${preset.dns.join("、")}？`
    )
  ) {
    return;
  }
  setBusy(true, "设置静态 IP…");
  try {
    const res = await invoke("set_static_ip", {
      service: selectedService,
      ip: preset.ip,
      netmask: preset.netmask,
      gateway: preset.gateway,
      dns: preset.dns,
    });
    setStatus(res.message, "ok");
    await refreshInterfaces();
  } catch (err) {
    setStatus(`设置失败：${err}`, "error");
  } finally {
    setBusy(false);
  }
});

applyDhcpBtn.addEventListener("click", async () => {
  if (!selectedService) return;
  if (!confirm(`确定将「${selectedService}」切换为 DHCP 自动获取 IP？\n当前手动 DNS 将被重置。`)) {
    return;
  }
  setBusy(true, "切换 DHCP…");
  try {
    const res = await invoke("set_dhcp", { service: selectedService });
    setStatus(res.message, "ok");
    await refreshInterfaces();
  } catch (err) {
    setStatus(`切换失败：${err}`, "error");
  } finally {
    setBusy(false);
  }
});

$("refresh-btn").addEventListener("click", async () => {
  const ok = await refreshInterfaces();
  recordLog(
    "refresh_interfaces",
    `当前发现 ${interfaces.length} 张物理网卡`,
    ok ? "success" : "fail",
    ok ? "刷新完成" : "刷新失败"
  );
});

// ---------------------------------------------------------------------------
// 初始化
// ---------------------------------------------------------------------------
(async function init() {
  await refreshInterfaces();
  await refreshPresets();
})();