interface MeterProps {
  used: number;
  total: number;
  label: string;
}

const CAUTION_RATIO = 0.8;

export default function Meter({ used, total, label }: MeterProps) {
  const ratio = total > 0 ? Math.min(1, used / total) : 0;
  const tone = ratio >= CAUTION_RATIO ? "meter--caution" : "";
  return (
    <div className={`meter ${tone}`}>
      <div className="meter__track" role="meter" aria-valuemin={0} aria-valuemax={total} aria-valuenow={used} aria-label={label}>
        <span className="meter__fill" style={{ width: `${ratio * 100}%` }} />
      </div>
      <div className="meter__label">{label}</div>
    </div>
  );
}
