import { useEffect } from "react";

const DARK_SCHEME = "(prefers-color-scheme: dark)";

function applyTheme(isDark: boolean): void {
  document.body.classList.toggle("is-dark", isDark);
  document.body.classList.toggle("is-light", !isDark);
}

export function useSystemTheme(): void {
  useEffect(() => {
    const preference = window.matchMedia(DARK_SCHEME);
    applyTheme(preference.matches);
    const changed = (event: MediaQueryListEvent) => applyTheme(event.matches);
    preference.addEventListener("change", changed);
    return () => preference.removeEventListener("change", changed);
  }, []);
}
