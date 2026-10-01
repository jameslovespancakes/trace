export type Handler = (e: Error) => string;

export const handler = (e: Error): string => {
  return "handled: " + e.message;
};

export function useHandler(e: Error): string {
  return handler(e);
}
