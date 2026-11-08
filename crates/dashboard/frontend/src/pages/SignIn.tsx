import { Button, Card, Form, Input, Notification } from "@canonical/react-components";
import { useState } from "react";
import type { FormEvent } from "react";

import { describeFailure } from "../cluster/client";
import { useSession } from "../session";

export default function SignIn() {
  const { signIn } = useSession();
  const [token, setToken] = useState("");
  const [project, setProject] = useState("");
  const [problem, setProblem] = useState<string | null>(null);
  const [isBusy, setBusy] = useState(false);

  const submitted = async (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setProblem(null);
    try {
      await signIn(token, project.trim() || null);
    } catch (failure) {
      setProblem(describeFailure(failure));
    } finally {
      setBusy(false);
    }
  };

  return (
    <main className="sign-in">
      <Card className="sign-in__card">
        <h1 className="p-heading--3">Sign in to Amafu</h1>
        <p className="u-text--muted">
          The dashboard forwards every request with your own token, so it can do only what you may.
        </p>
        {problem && (
          <Notification severity="negative" title="Not signed in">
            {problem}
          </Notification>
        )}
        <Form onSubmit={submitted}>
          <Input
            id="token"
            label="Token"
            type="password"
            autoComplete="off"
            required
            value={token}
            onChange={(event) => setToken(event.target.value)}
          />
          <Input type="text"
            id="project"
            label="Project"
            help="Leave empty to use your token's own project."
            value={project}
            onChange={(event) => setProject(event.target.value)}
          />
          <Button type="submit" appearance="positive" disabled={isBusy}>
            Sign in
          </Button>
        </Form>
      </Card>
    </main>
  );
}
