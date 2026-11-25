import { providerIcon } from "../lib/providerIcons";

/** The service's own mark where we have it, else its initials on a tint. */
export function Mark({ id, name, size = 28 }: { id: string; name: string; size?: number }) {
  const icon = providerIcon(id);
  if (icon) return <span className="ms-mark has-icon" style={{ width: size, height: size }} aria-hidden dangerouslySetInnerHTML={{ __html: icon }} />;
  const letters = name.replace(/[（(].*$/, "").trim().split(/\s+/).map((w) => w[0]).join("").slice(0, 2).toUpperCase();
  return <span className="ms-mark" style={{ width: size, height: size, fontSize: size * 0.4 }} aria-hidden>{letters}</span>;
}
