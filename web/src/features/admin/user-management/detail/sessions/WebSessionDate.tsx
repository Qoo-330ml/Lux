export function WebSessionDate({ value }: { value: number }) {
  const date = new Date(value * 1000);
  if (!Number.isFinite(date.getTime())) return <span>—</span>;
  return (
    <time dateTime={date.toISOString()}>
      {new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(date)}
    </time>
  );
}
