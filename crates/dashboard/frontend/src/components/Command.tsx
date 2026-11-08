interface CommandProps {
  words: (string | false | null | undefined)[];
  title?: string;
}

const LINE_LENGTH = 40;

function quoted(word: string): string {
  return /^[\w@%+=:,./-]+$/.test(word) ? word : `'${word.replaceAll("'", "'\\''")}'`;
}

export function commandLines(words: string[]): string[] {
  const groups: string[][] = [];
  for (const word of words.map(quoted)) {
    if (groups.length === 0 || word.startsWith("-")) {
      groups.push([word]);
    } else {
      groups[groups.length - 1].push(word);
    }
  }
  const whole = `$ ${groups.map((group) => group.join(" ")).join(" ")}`;
  if (whole.length <= LINE_LENGTH) {
    return [whole];
  }
  const lines = groups.map((group, index) => `${index === 0 ? "$" : "   "} ${group.join(" ")}`);
  return lines.map((line, index) => (index < lines.length - 1 ? `${line} \\` : line));
}

export default function Command({ words, title = "Same as CLI" }: CommandProps) {
  const present = words.filter((word): word is string => typeof word === "string" && word.length > 0);
  return (
    <div className="same-as-command">
      <h3 className="p-text--small-caps">{title}</h3>
      <pre className="same-as-command__code">
        <code>{commandLines(present).join("\n")}</code>
      </pre>
    </div>
  );
}
