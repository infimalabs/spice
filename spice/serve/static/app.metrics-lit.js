const litCoreUrl = "https://cdn.jsdelivr.net/gh/lit/dist@3/core/lit-core.min.js";
const litCore = await import(litCoreUrl);
/** @type {typeof HTMLElement} */
const LitElement = litCore.LitElement;
/** @type {(strings: TemplateStringsArray, ...values: unknown[]) => unknown} */
const html = litCore.html;

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
      ${this.renderSeriesChart(((model.series || {}).points) || [])}
      ${this.renderSeriesControls(model.seriesControls || {})}
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

  renderSeriesControls(controls) {
    return html`
      <span class="lane-metric-series-controls lane-metric-cell--wide">
        ${this.renderSeriesSelect("metric", controls.metric, controls.metrics || [])}
        ${this.renderSeriesSelect("lens", controls.lens, controls.lenses || [])}
        ${this.renderSeriesSelect(
          "rangeSeconds",
          controls.rangeSeconds,
          controls.ranges || [],
        )}
      </span>
    `;
  }

  renderSeriesSelect(name, selectedValue, options) {
    return html`
      <select
        class="lane-metric-series-select"
        aria-label=${"Metric " + name}
        @change=${(event) => this.updateSeriesSetting(name, event)}
      >
        ${options.map(
          ([value, label]) => html`
            <option value=${value} ?selected=${String(value) === String(selectedValue || "")}>
              ${label}
            </option>
          `,
        )}
      </select>
    `;
  }

  updateSeriesSetting(name, event) {
    this.dispatchEvent(
      new CustomEvent("spice-metric-series-change", {
        bubbles: true,
        detail: { [name]: event.target.value },
      }),
    );
  }

  renderSeriesChart(points) {
    const values = points.map((point) => Math.max(0, Number(point.value) || 0));
    if (!values.length)
      return html`
        <span class="lane-metric-series-chart lane-metric-cell--wide">
          <span class="lane-metric-series-empty">no series</span>
        </span>
      `;
    const max = points.some((point) => typeof point.share !== "undefined")
      ? 1
      : Math.max(1, ...values);
    const width = 120;
    const height = 36;
    const buckets = this.seriesBuckets(points);
    const bucketIndex = new Map(buckets.map((bucket, index) => [bucket, index]));
    const step = buckets.length > 1 ? width / (buckets.length - 1) : width;
    return html`
      <span class="lane-metric-series-chart lane-metric-cell--wide">
        <svg
          class="lane-metric-series-svg"
          viewBox="0 0 120 36"
          role="img"
          aria-label="Metric series"
        >
          ${this.seriesPointGroups(points).map(
            (group, seriesIndex) => {
              const coords = group.points.map((point) => {
                const bucket = this.seriesPointBucket(point);
                const index = bucketIndex.has(bucket) ? bucketIndex.get(bucket) || 0 : 0;
                const x = buckets.length > 1 ? index * step : width / 2;
                const value = Math.max(0, Number(point.value) || 0);
                const y = height - (value / max) * (height - 4) - 2;
                return [x, y, point];
              });
              const path = coords
                .map(([x, y]) => x.toFixed(1) + "," + y.toFixed(1))
                .join(" ");
              return html`
                <g
                  class="lane-metric-series-group"
                  data-agent-id=${group.agentId || ""}
                  style=${"--lane-metric-series-color: " + this.seriesColor(seriesIndex)}
                >
                  <title>${group.agentId || "series"}</title>
                  <polyline class="lane-metric-series-line" points=${path}></polyline>
                  ${coords.map(
                    ([x, y, point]) => html`
                      <circle
                        class="lane-metric-series-dot"
                        data-agent-id=${point.agentId || ""}
                        cx=${x.toFixed(1)}
                        cy=${y.toFixed(1)}
                        r="1.8"
                      ></circle>
                    `,
                  )}
                </g>
              `;
            },
          )}
        </svg>
      </span>
    `;
  }

  seriesPointGroups(points) {
    const groups = new Map();
    for (const point of points) {
      const agentId = String(point.agentId || "");
      const key = agentId || "series";
      if (!groups.has(key)) groups.set(key, { agentId, points: [] });
      groups.get(key).points.push(point);
    }
    return Array.from(groups.values());
  }

  seriesBuckets(points) {
    const buckets = new Set();
    for (const point of points) buckets.add(this.seriesPointBucket(point));
    return Array.from(buckets).sort((left, right) => left - right);
  }

  seriesPointBucket(point) {
    return Number.isFinite(Number(point.bucketStart)) ? Number(point.bucketStart) : 0;
  }

  seriesColor(index) {
    const colors = ["#1677ff", "#d9480f", "#2f9e44", "#ae3ec9", "#0ca678", "#f08c00"];
    return colors[index % colors.length];
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
