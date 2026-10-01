// Fixture (bridges gate, aspnet family): attribute routes.
using Microsoft.AspNetCore.Mvc;

[Route("api/orders")]
public class OrdersController : ControllerBase
{
    [HttpGet("{id}")]
    public string Get(string id) => id;
}
