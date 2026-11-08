import { Link, useLocation } from "react-router";

interface SubjectLinkProps {
  name: string;
  panel?: string;
  page?: string;
}

export default function SubjectLink({ name, panel = "detail", page }: SubjectLinkProps) {
  const location = useLocation();
  const parameters = new URLSearchParams({ panel, subject: name });
  return <Link to={`${page ?? location.pathname}?${parameters.toString()}`}>{name}</Link>;
}
