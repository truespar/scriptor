// The OOXML schema-validation gate.
//
// Validates every .docx given (files, or directories walked recursively) with the Open XML SDK's
// OpenXmlValidator and exits non-zero if any document has schema errors. This is the cheap,
// reference-renderer-free conformance check for everything Scriptor WRITES: run it over
// `roundtrip`/`remodel`/`compare`/`accept`/`reject` outputs. A namespace-non-well-formed or
// schema-invalid part fails here immediately - no Word required.
//
//   dotnet run --project tools/ooxml-validate -c Release -- <path> [<path>...] [--json out.json] [--max-errors N]
//
// Word owner-lock artifacts (~$*.docx) are skipped. Files that cannot be opened as an OPC package
// (encrypted CFB containers, truncated zips) are reported as unreadable but do not fail the gate -
// mirroring `scriptor roundtrip <dir>`: the gate's promise covers documents we can open.

using System.Text.Json;
using DocumentFormat.OpenXml;
using DocumentFormat.OpenXml.Packaging;
using DocumentFormat.OpenXml.Validation;

var paths = new List<string>();
string? jsonOut = null;
var maxErrors = 20;

for (var i = 0; i < args.Length; i++)
{
    switch (args[i])
    {
        case "--json" when i + 1 < args.Length:
            jsonOut = args[++i];
            break;
        case "--max-errors" when i + 1 < args.Length:
            maxErrors = int.Parse(args[++i]);
            break;
        case "--help" or "-h":
            Console.WriteLine("usage: ooxml-validate <file-or-dir>... [--json out.json] [--max-errors N]");
            return 0;
        default:
            paths.Add(args[i]);
            break;
    }
}

if (paths.Count == 0)
{
    Console.Error.WriteLine("error: no input paths (use --help)");
    return 2;
}

var files = new List<(string Full, string Rel)>();
foreach (var p in paths)
{
    if (Directory.Exists(p))
    {
        foreach (var f in Directory.EnumerateFiles(p, "*.docx", SearchOption.AllDirectories)
                     .Where(f => !Path.GetFileName(f).StartsWith("~$"))
                     .OrderBy(f => f, StringComparer.OrdinalIgnoreCase))
        {
            files.Add((f, Path.GetRelativePath(p, f).Replace('\\', '/')));
        }
    }
    else if (File.Exists(p))
    {
        files.Add((p, Path.GetFileName(p)));
    }
    else
    {
        Console.Error.WriteLine($"error: no such path: {p}");
        return 2;
    }
}

if (files.Count == 0)
{
    Console.Error.WriteLine("error: no .docx files found");
    return 2;
}

// Validate against the newest schema set the SDK knows: older targets would flag legitimate
// modern markup (w15 comment threading, w16 extensions) that Word itself writes.
var validator = new OpenXmlValidator(FileFormatVersions.Microsoft365);

var docs = new List<DocResult>();
int valid = 0, invalid = 0, unreadable = 0;

foreach (var (full, rel) in files)
{
    try
    {
        using var doc = WordprocessingDocument.Open(full, isEditable: false);
        List<SchemaError> errors;
        try
        {
            errors = validator.Validate(doc)
                .Select(e => new SchemaError(
                    e.Description,
                    e.Part?.Uri.ToString() ?? "",
                    e.Path?.XPath ?? "",
                    e.Node?.LocalName ?? ""))
                .ToList();
        }
        catch (Exception ex)
        {
            // The validator itself throws on broken package structure (e.g. a relationship
            // pointing at a part that is not in the zip) instead of reporting it - that is a
            // conformance failure of the DOCUMENT, not of the gate, so record it as invalid
            // rather than crashing the whole corpus run.
            errors = [new SchemaError($"package validation threw: {ex.Message}", "", "", "")];
        }
        if (errors.Count == 0)
        {
            valid++;
            docs.Add(new DocResult(rel, "valid", errors));
        }
        else
        {
            invalid++;
            docs.Add(new DocResult(rel, "invalid", errors));
            Console.WriteLine($"INVALID     {rel}  ({errors.Count} error(s))");
            foreach (var e in errors.Take(maxErrors))
            {
                Console.WriteLine($"    [{e.Part}] {e.Description}");
                if (e.XPath.Length > 0)
                {
                    Console.WriteLine($"      at {e.XPath}");
                }
            }
            if (errors.Count > maxErrors)
            {
                Console.WriteLine($"    ... and {errors.Count - maxErrors} more");
            }
        }
    }
    catch (Exception ex) when (ex is OpenXmlPackageException or FileFormatException or InvalidDataException or IOException)
    {
        unreadable++;
        docs.Add(new DocResult(rel, "unreadable", [new SchemaError(ex.Message, "", "", "")]));
        Console.WriteLine($"UNREADABLE  {rel}  ({ex.Message})");
    }
}

Console.WriteLine($"validated {files.Count} file(s)  -  valid: {valid}   invalid: {invalid}   unreadable: {unreadable}");

if (jsonOut is not null)
{
    var summary = new Summary(files.Count, valid, invalid, unreadable, docs);
    File.WriteAllText(jsonOut, JsonSerializer.Serialize(summary, JsonContext.Default.Summary));
    Console.WriteLine($"results: {jsonOut}");
}

return invalid > 0 ? 1 : 0;

internal sealed record SchemaError(string Description, string Part, string XPath, string Node);

internal sealed record DocResult(string File, string Status, List<SchemaError> Errors);

internal sealed record Summary(int Scanned, int Valid, int Invalid, int Unreadable, List<DocResult> Docs);

[System.Text.Json.Serialization.JsonSourceGenerationOptions(WriteIndented = true, PropertyNamingPolicy = System.Text.Json.Serialization.JsonKnownNamingPolicy.CamelCase)]
[System.Text.Json.Serialization.JsonSerializable(typeof(Summary))]
internal sealed partial class JsonContext : System.Text.Json.Serialization.JsonSerializerContext;
