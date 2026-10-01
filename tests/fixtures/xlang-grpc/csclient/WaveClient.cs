// Fixture (round 2, grpc): C# client whose generated client class is imported with
// `using static Helloworld.Greeter;` (unqualified `new GreeterClient(channel)`).
using System.Threading.Tasks;
using Grpc.Net.Client;
using static Helloworld.Greeter;

namespace Fixture
{
    public class WaveClient
    {
        public async Task Run(GrpcChannel channel)
        {
            var client = new GreeterClient(channel);
            await client.WaveAsync(new Helloworld.HelloRequest()); // two servicer subclasses -> possible
        }
    }
}
