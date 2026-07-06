import { For, Show } from "solid-js";
import { t } from "./i18n";
import type { FeedItem } from "./types";
import { TechText } from "./TechText";

interface Props {
  items: FeedItem[];
  onDecide: (request_id: string, decision: "allow" | "deny") => void;
  onDismiss: (request_id: string) => void;
}

// v0.4.4 (i18n): the card's kind / subkind / state were rendered as raw
// English codes ("pre-tool-use", "denied"). Translate them, falling back to
// the raw code for any value without a key (t() returns the key itself on a
// miss, so key-equality detects that).
function tr(key: string, raw: string): string {
  const v = t(key);
  return v === key ? raw : v;
}

export function FeedPanel(p: Props) {
  const subkindLabel = (item: FeedItem): string =>
    item.subkind
      ? tr(`feed.subkind.${item.subkind}`, item.subkind)
      : tr(`feed.kind.${item.kind}`, item.kind);
  const stateLabel = (item: FeedItem): string =>
    tr(`feed.state.${item.state}`, item.state);
  return (
    <Show when={p.items.length > 0}>
      <div class="feed-panel">
        <For each={p.items}>
          {(item) => (
            <div
              class={`feed-card feed-state-${item.state} ${item.blocking ? "blocking" : ""}`}
            >
              <div class="feed-head">
                <span class={`feed-kind feed-kind-${item.kind}`}>
                  {subkindLabel(item)}
                </span>
                <Show when={item.state !== "pending"}>
                  <span class={`feed-verdict feed-verdict-${item.state}`}>
                    {stateLabel(item)}
                  </span>
                </Show>
                <button
                  class="feed-x"
                  title={t("feed.btn.dismiss")}
                  onClick={() => p.onDismiss(item.request_id)}
                >
                  ×
                </button>
              </div>
              <div class="feed-title"><TechText text={item.title} /></div>
              <Show when={item.summary}>
                <div class="feed-summary">{item.summary}</div>
              </Show>
              <Show when={item.state === "pending" && item.blocking}>
                <div class="feed-actions">
                  <button
                    class="feed-allow"
                    onClick={() => p.onDecide(item.request_id, "allow")}
                  >
                    {t("feed.allow")}
                  </button>
                  <button
                    class="feed-deny"
                    onClick={() => p.onDecide(item.request_id, "deny")}
                  >
                    {t("feed.deny")}
                  </button>
                </div>
              </Show>
            </div>
          )}
        </For>
      </div>
    </Show>
  );
}
