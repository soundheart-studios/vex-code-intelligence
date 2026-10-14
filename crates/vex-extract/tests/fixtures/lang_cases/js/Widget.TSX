export function Widget(props: { label: string }) {
  return <div onClick={() => props.label}>{props.label}</div>;
}
