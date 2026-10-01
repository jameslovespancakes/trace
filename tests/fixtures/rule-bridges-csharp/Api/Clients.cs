// Fixture (bridges gate, dotnet-httpclient family).
using System.Net.Http;
using System.Threading.Tasks;

public class Clients
{
    private readonly HttpClient client = new HttpClient();

    public Task<HttpResponseMessage> Load() => client.GetAsync("http://svc.local/min/users/1");
}
