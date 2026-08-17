declare module "ebml" {
  export type EbmlElement = {
    name: string;
    type: string;
    data?: Uint8Array;
    value?: number | string;
  };

  export class Decoder {
    on(event: "data", listener: (chunk: ["start" | "tag" | "end", EbmlElement]) => void): this;
    on(event: "error", listener: (error: Error) => void): this;
    write(chunk: Uint8Array): boolean;
    end(): void;
  }
}
