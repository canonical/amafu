import type { ReactNode } from "react";

interface PageHeaderProps {
  title: ReactNode;
  children?: ReactNode;
}

export default function PageHeader({ title, children }: PageHeaderProps) {
  return (
    <header className="page-header">
      <h1 className="p-heading--4 u-no-margin--bottom page-header__title">{title}</h1>
      <div className="page-header__controls">{children}</div>
    </header>
  );
}
