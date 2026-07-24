// Built-in test payloads. Not secrets — a one-page PDF and a one-line ZPL label.
// Gotcha #1: never send PDF bytes to a label printer, it prints blanks. Pick by
// the printer's can_pdf flag.
export const TEST_PDF_B64 =
  "JVBERi0xLjQKMSAwIG9iago8PCAvVHlwZSAvQ2F0YWxvZyAvUGFnZXMgMiAwIFIgPj4KZW5kb2JqCjIg" +
  "MCBvYmoKPDwgL1R5cGUgL1BhZ2VzIC9LaWRzIFszIDAgUl0gL0NvdW50IDEgPj4KZW5kb2JqCjMgMCBv" +
  "YmoKPDwgL1R5cGUgL1BhZ2UgL1BhcmVudCAyIDAgUiAvTWVkaWFCb3ggWzAgMCA2MTIgNzkyXSAvUmVz" +
  "b3VyY2VzIDw8IC9Gb250IDw8IC9GMSA0IDAgUiA+PiA+PiAvQ29udGVudHMgNSAwIFIgPj4KZW5kb2Jq" +
  "CjQgMCBvYmoKPDwgL1R5cGUgL0ZvbnQgL1N1YnR5cGUgL1R5cGUxIC9CYXNlRm9udCAvSGVsdmV0aWNh" +
  "ID4+CmVuZG9iago1IDAgb2JqCjw8IC9MZW5ndGggNTAgPj4Kc3RyZWFtCkJUIC9GMSAyNCBUZiA3MiA3" +
  "MDAgVGQgKHByaW50cGFwaSB0ZXN0IHBhZ2UpIFRqIEVUCmVuZHN0cmVhbQplbmRvYmoKeHJlZgowIDYK" +
  "MDAwMDAwMDAwMCA2NTUzNSBmIAowMDAwMDAwMDA5IDAwMDAwIG4gCjAwMDAwMDAwNTggMDAwMDAgbiAK" +
  "MDAwMDAwMDExNSAwMDAwMCBuIAowMDAwMDAwMjQxIDAwMDAwIG4gCjAwMDAwMDAzMTEgMDAwMDAgbiAK" +
  "dHJhaWxlcgo8PCAvU2l6ZSA2IC9Sb290IDEgMCBSID4+CnN0YXJ0eHJlZgo0MTEKJSVFT0Y=";

export const TEST_ZPL_B64 = "XlhBXkZPNDAsNDBeQUROLDM2LDIwXkZEcHJpbnRwYXBpIHRlc3ReRlNeWFo=";

/** The job body for a test print on a given printer. */
export function testJob(printerId: number, canPdf: boolean) {
  return canPdf
    ? { printer_id: printerId, type: "pdf_base64" as const, content: TEST_PDF_B64, title: "test page" }
    : { printer_id: printerId, type: "raw_base64" as const, content: TEST_ZPL_B64, title: "test label" };
}
