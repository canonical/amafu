import type { ReactNode } from "react";

interface TagProps {
  children: ReactNode;
  emphasis?: "information" | "plain";
}

export default function Tag({ children, emphasis = "plain" }: TagProps) {
  return <span className={`amafu-tag amafu-tag--${emphasis}`}>{children}</span>;
}
