// Package files will serve the shared-folder + directory-picker API
// (/api/v2/files/*): list, upload, download — root-scoped and
// path-traversal-safe (PHASE-77-DESIGN §4.2). Compile-only stub in Sprint 1;
// implemented in Sprint 2. Open question Q2 (single root vs full-FS) is an S2
// decision.
package files

// TODO(S2): List(root, path) with root-escape rejection; Upload; Download (Range).
