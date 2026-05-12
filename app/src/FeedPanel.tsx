import { For, Show } from "solid-js";
import { t } from "./i18n";
import type { FeedItem } from "./types";

interface Props {
  items: FeedItem[];
  onDecide: (request_id: string, decision: "allow" | "deny") => void;
  onDismiss: (request_id: string) => void;
}

export function FeedPanel(p: Props) {
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
                  {item.subkind || item.kind}
                </span>
                <Show when={item.state !== "pending"}>
                  <span class={`feed-verdict feed-verdict-${item.state}`}>
                    {item.state}
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
              <div class="feed-title">{item.title}</div>
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
