import type { ReactNode } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";

/** Opens in the system browser (webview links would navigate the app itself). */
export function ExternalLink({ href, children }: { href: string; children: ReactNode }) {
  return (
    <a
      href={href}
      onClick={(e) => {
        e.preventDefault();
        openUrl(href).catch(() => {});
      }}
    >
      {children}
    </a>
  );
}
