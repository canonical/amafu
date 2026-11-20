interface Segment<Value extends string> {
  value: Value;
  label: string;
}

interface SegmentsProps<Value extends string> {
  label: string;
  value: Value;
  segments: Segment<Value>[];
  onChange: (value: Value) => void;
}

export default function Segments<Value extends string>({ label, value, segments, onChange }: SegmentsProps<Value>) {
  return (
    <div className="segments">
      <span className="p-form__label segments__label">{label}</span>
      <div className="p-segmented-control">
        <div className="p-segmented-control__list" role="tablist" aria-label={label}>
          {segments.map((segment) => (
            <button
              key={segment.value}
              type="button"
              role="tab"
              className="p-segmented-control__button"
              aria-selected={segment.value === value}
              onClick={() => onChange(segment.value)}
            >
              {segment.label}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
