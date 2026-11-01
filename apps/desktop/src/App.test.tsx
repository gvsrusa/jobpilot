import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import App from "./App";

describe("App", () => {
  it("renders_app_title", () => {
    render(<App />);
    expect(screen.getByText("JobPilot AI")).toBeInTheDocument();
  });
});
