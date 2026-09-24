#ifndef MS_RTC_MEDIA_DIAGNOSTIC_ID_HPP
#define MS_RTC_MEDIA_DIAGNOSTIC_ID_HPP

#include <algorithm>
#include <array>
#include <string_view>

namespace RTC::MediaDiagnostics
{
	// Native IDs are private operational correlation, never endpoint addresses or
	// client-provided text. Keep their storage and log representation strictly bounded.
	class Identifier
	{
	public:
		explicit Identifier(std::string_view value = {})
		{
			if (value.size() != 36)
			{
				return;
			}
			for (size_t i{0}; i < value.size(); ++i)
			{
				const auto c = value[i];
				const bool separator = i == 8 || i == 13 || i == 18 || i == 23;
				if (separator ? c != '-' : !((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f')))
				{
					return;
				}
			}
			std::copy(value.begin(), value.end(), this->value.begin());
		}

		const char *Get() const
		{
			return this->value[0] ? this->value.data() : "unavailable";
		}

	private:
		std::array<char, 37> value{};
	};

} // namespace RTC::MediaDiagnostics

#endif
