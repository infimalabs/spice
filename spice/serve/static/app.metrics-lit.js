const litCoreUrl = "https://cdn.jsdelivr.net/gh/lit/dist@3/core/lit-core.min.js";
const { LitElement, html } = await import(litCoreUrl);

class SpiceLaneMetricsElement extends LitElement {
  static properties = {
    model: { state: true },
  };

  constructor() {
    super();
    this.model = { cells: [], sparkline: [], activityTotal: 0 };
  }

  createRenderRoot() {
    return this;
  }

  render() {
    const model = this.model || {};
    const cells = Array.isArray(model.cells) ? model.cells : [];
    const sparkline = Array.isArray(model.sparkline) ? model.sparkline : [];
    const max = Math.max(1, ...sparkline);
    return html`
      ${cells.map((cell) => this.renderCell(cell.label, cell.value))}
      <span class="lane-metric-cell lane-metric-cell--wide">
        <span class="lane-metric-value">${model.activityTotal || 0} messages</span>
        <span class="lane-metric-label">activity</span>
        <div class="lane-metric-sparkline">
          ${sparkline.map((value) => this.renderSparklineBar(value, max))}
        </div>
      </span>
    `;
  }

  renderCell(label, value) {
    return html`
      <span class="lane-metric-cell">
        <span class="lane-metric-value">${value}</span>
        <span class="lane-metric-label">${label}</span>
      </span>
    `;
  }

  renderSparklineBar(value, max) {
    const level = Math.max(1, Math.ceil((Number(value || 0) / max) * 8));
    return html`
      <span
        class="lane-metric-sparkline-bar"
        style="--lane-metric-sparkline-level: ${level}"
      ></span>
    `;
  }
}

if (!customElements.get("spice-lane-metrics")) {
  customElements.define("spice-lane-metrics", SpiceLaneMetricsElement);
}

export function renderLaneMetricsLitIsland(host, model) {
  let island = host.querySelector("spice-lane-metrics");
  if (!island) {
    island = document.createElement("spice-lane-metrics");
    island.className = "lane-metrics-lit-island";
    host.replaceChildren(island);
  }
  island.model = model;
}
