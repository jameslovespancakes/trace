// Fixture (bridges gate, aspnet-minimal family): a minimal-API route.
var builder = WebApplication.CreateBuilder(args);
var app = builder.Build();

app.MapGet("/min/users/{id}", (string id) => id);

app.Run();
